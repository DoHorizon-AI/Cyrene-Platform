"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 client.py                                                        │
│  Module: cyrene_runtime_maintenance.client                           │
│  Role: Authenticated JSON IPC client for the runtime maintenance gate.│
│                                                                      │
│  模块职责：通过 Unix socket 调用运行时更新门禁与任务活动接口。          │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import os
import re
import socket
import uuid
from pathlib import Path
from typing import Any, Iterable, Mapping


class MaintenanceError(RuntimeError):
    """A broker rejection or unavailable IPC endpoint with a stable code."""

    def __init__(self, code: str, message: str, *, readiness_status: str = "UNKNOWN") -> None:
        super().__init__(message)
        self.code = code
        self.readiness_status = readiness_status


class RuntimeMaintenanceClient:
    """Calls the authenticated local maintenance broker over a Unix socket.

    Args:
        socket_path: Broker socket path shared with the caller.
        source_id: Trusted catalog source ID for Product activity calls.
        source_token: Read-only per-source token from a mounted secret file.
        operator_token: Root-only capability used by a privileged updater.
        catalog_generation: Installed source catalog generation reported by the installer.
        timeout_seconds: Per-request connect/read timeout.
    """

    def __init__(
        self,
        socket_path: str | os.PathLike[str] = "/run/cyrene/runtime-maintenance.sock",
        *,
        source_id: str | None = None,
        source_token: str | None = None,
        operator_token: str | None = None,
        catalog_generation: int | None = None,
        timeout_seconds: float = 5.0,
    ) -> None:
        self.socket_path = str(socket_path)
        self.source_id = source_id
        self.source_token = source_token.strip() if source_token else None
        self.operator_token = operator_token.strip() if operator_token else None
        self.catalog_generation = catalog_generation
        self.timeout_seconds = timeout_seconds
        if (source_id is None) != (source_token is None):
            raise ValueError("source_id and source_token must be supplied together")

    @classmethod
    def from_source_secret(
        cls,
        source_id: str,
        token_path: str | os.PathLike[str],
        *,
        catalog_generation: int,
        socket_path: str | os.PathLike[str] = "/run/cyrene/runtime-maintenance.sock",
        timeout_seconds: float = 5.0,
    ) -> RuntimeMaintenanceClient:
        """Loads one source's read-only secret file without exposing other source tokens."""
        token = Path(token_path).read_text(encoding="utf-8").strip()
        if not token:
            raise ValueError("source token file is empty")
        return cls(
            socket_path,
            source_id=source_id,
            source_token=token,
            catalog_generation=catalog_generation,
            timeout_seconds=timeout_seconds,
        )

    @classmethod
    def as_operator(
        cls,
        token_path: str | os.PathLike[str],
        *,
        socket_path: str | os.PathLike[str] = "/run/cyrene/runtime-maintenance.sock",
        catalog_generation: int | None = None,
        timeout_seconds: float = 5.0,
    ) -> RuntimeMaintenanceClient:
        """Loads the private operator token for a root-owned updater process."""
        token = Path(token_path).read_text(encoding="utf-8").strip()
        if not token:
            raise ValueError("operator token file is empty")
        return cls(
            socket_path,
            operator_token=token,
            catalog_generation=catalog_generation,
            timeout_seconds=timeout_seconds,
        )

    @classmethod
    def from_environment(cls) -> RuntimeMaintenanceClient:
        """Builds a Product client from the installer-provided source settings."""
        source_id = os.environ.get("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID", "").strip()
        token_path = os.environ.get(
            "CYRENE_RUNTIME_ACTIVITY_SOURCE_TOKEN_FILE",
            "/run/secrets/cyrene-runtime-activity-token",
        )
        generation = os.environ.get("CYRENE_RUNTIME_ACTIVITY_CATALOG_GENERATION", "")
        socket_path = os.environ.get(
            "CYRENE_RUNTIME_MAINTENANCE_SOCKET",
            "/run/cyrene/runtime-maintenance.sock",
        )
        try:
            catalog_generation = int(generation)
        except ValueError as error:
            raise ValueError("CYRENE_RUNTIME_ACTIVITY_CATALOG_GENERATION must be an integer") from error
        if not source_id:
            raise ValueError("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID is required")
        return cls.from_source_secret(
            source_id,
            token_path,
            catalog_generation=catalog_generation,
            socket_path=socket_path,
        )

    def get_update_readiness(
        self,
        *,
        target_kind: str = "PACKAGE_ONLY",
        expected_activity_sources: Iterable[str],
        expected_catalog_generation: int | None = None,
        requires_restart: bool = True,
        plan_id: str,
        plan_digest: str,
        component_artifact_digests: Mapping[str, str],
    ) -> dict[str, Any]:
        """Returns a complete trusted-source readiness snapshot.

        An empty, incomplete, or stale source set is reported as `UNKNOWN` by the broker.
        """
        generation = self._catalog_generation(expected_catalog_generation)
        return self._request(
            "GetUpdateReadiness",
            {
                "target_kind": target_kind,
                "requires_restart": requires_restart,
                "expected_catalog_generation": generation,
                "expected_activity_sources": sorted(set(expected_activity_sources)),
            },
        )

    def begin_maintenance(
        self,
        request_id: str,
        *,
        target_kind: str = "PACKAGE_ONLY",
        expected_gate_generation: int,
        expected_activity_sources: Iterable[str],
        user_confirmed_restart: bool,
        expected_catalog_generation: int | None = None,
        requires_restart: bool = True,
        plan_id: str,
        plan_digest: str,
        component_artifact_digests: Mapping[str, str],
    ) -> dict[str, Any]:
        """Atomically rechecks readiness and closes new task/runtime admission.

        Args:
            request_id: Stable maintenance transaction ID. Reuse this value for
                retries and the matching ``end_maintenance`` call. The outer
                JSON IPC correlation ID is generated separately for each call.

        中文：该值写入 `params.request_id` 并跨 Begin/End 复用；外层 correlation ID 每次调用单独生成。
        """
        generation = self._catalog_generation(expected_catalog_generation)
        if not plan_id:
            raise ValueError("plan_id is required")
        _validate_plan_digest(plan_digest, "plan_digest")
        if not component_artifact_digests:
            raise ValueError("component_artifact_digests must contain at least one artifact")
        for component_id, digest in component_artifact_digests.items():
            if not component_id:
                raise ValueError("component IDs must be non-empty")
            _validate_plan_digest(digest, f"artifact digest for {component_id}")
        return self._request(
            "BeginMaintenance",
            {
                "request_id": request_id,
                "target_kind": target_kind,
                "requires_restart": requires_restart,
                "expected_gate_generation": expected_gate_generation,
                "expected_catalog_generation": generation,
                "expected_activity_sources": sorted(set(expected_activity_sources)),
                "user_confirmed_restart": user_confirmed_restart,
                "plan_id": plan_id,
                "plan_digest": plan_digest,
                "component_artifact_digests": dict(component_artifact_digests),
            },
        )

    def end_maintenance(
        self,
        request_id: str,
        maintenance_token: str,
        *,
        outcome: str,
        healthy: bool,
        target_kind: str = "PACKAGE_ONLY",
    ) -> dict[str, Any]:
        """Records apply/rollback health and unlocks only on healthy completion.

        Args:
            request_id: The original BeginMaintenance transaction ID, not an
                IPC correlation ID. The SDK generates a new outer correlation
                ID for this EndMaintenance request.

        中文：传入原 Begin 事务 ID；不要传入外层请求/响应 correlation ID。
        """
        self._require_operator()
        return self._request(
            "EndMaintenance",
            {
                "request_id": request_id,
                "maintenance_token": maintenance_token,
                "outcome": outcome,
                "healthy": healthy,
                "target_kind": target_kind,
            },
        )

    def heartbeat_activity_source(self) -> dict[str, Any]:
        """Refreshes this installed source's liveness using its own source token."""
        source_id, _source_token = self._require_source()
        return self._request(
            "HeartbeatActivitySource",
            {
                "source_id": source_id,
                "expected_catalog_generation": self._catalog_generation(None),
            },
        )

    def admit_task(self, task_id: str, *, state: str = "ACCEPTED") -> dict[str, Any]:
        """Durably reserves one task before its Product database accepts it."""
        source_id, _source_token = self._require_source()
        return self._request(
            "AdmitTask",
            {
                "source_id": source_id,
                "task_id": task_id,
                "state": state,
                "idempotency_key": f"{source_id}:{task_id}",
            },
        )

    def update_task_activity(self, task_id: str, *, state: str) -> dict[str, Any]:
        """Persists a task's nonterminal lifecycle transition before dispatch continues."""
        source_id, _source_token = self._require_source()
        return self._request(
            "UpdateTaskActivity",
            {
                "source_id": source_id,
                "task_id": task_id,
                "state": state,
                "idempotency_key": f"{source_id}:{task_id}:{state}",
            },
        )

    def complete_task(self, task_id: str) -> dict[str, Any]:
        """Removes a task only after its Product state is durably terminal."""
        source_id, _source_token = self._require_source()
        return self._request(
            "CompleteTask",
            {
                "source_id": source_id,
                "task_id": task_id,
            },
        )

    def list_active_tasks(self) -> list[dict[str, str]]:
        """Reads persisted active task IDs before Product restart reconciliation."""
        source_id, _source_token = self._require_source()
        result = self._request(
            "ListActiveTasks",
            {"source_id": source_id},
        )
        return result.get("active_tasks", [])

    def reconcile_activity_source(
        self, active_tasks: Iterable[Mapping[str, str]]
    ) -> dict[str, Any]:
        """Replaces this source's gate records with its durable nonterminal task snapshot."""
        source_id, _source_token = self._require_source()
        tasks = [{"task_id": task["task_id"], "state": task["state"]} for task in active_tasks]
        return self._request(
            "ReconcileActivitySource",
            {
                "source_id": source_id,
                "expected_catalog_generation": self._catalog_generation(None),
                "active_tasks": tasks,
            },
        )

    def _request(self, method: str, params: Mapping[str, Any]) -> dict[str, Any]:
        """Sends one bounded JSON-line request and validates its matching response ID."""
        request_id = str(uuid.uuid4())
        auth: dict[str, str] = {}
        if self.source_id and self.source_token:
            auth.update(source_id=self.source_id, source_token=self.source_token)
        if self.operator_token:
            auth["operator_token"] = self.operator_token
        request = {
            "request_id": request_id,
            "method": method,
            "auth": auth,
            "params": dict(params),
        }
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(self.timeout_seconds)
                connection.connect(self.socket_path)
                connection.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
                with connection.makefile("rb") as stream:
                    raw = stream.readline(1_048_577)
            if len(raw) > 1_048_576 or not raw.endswith(b"\n"):
                raise MaintenanceError(
                    "MAINTENANCE_PROTOCOL_INVALID", "broker response exceeded protocol bounds"
                )
            response = json.loads(raw)
        except MaintenanceError:
            raise
        except (OSError, json.JSONDecodeError) as error:
            raise MaintenanceError(
                "UPDATE_READINESS_UNKNOWN",
                f"runtime maintenance broker is unavailable: {error}",
                readiness_status="UNKNOWN",
            ) from error
        if response.get("request_id") != request_id:
            raise MaintenanceError(
                "MAINTENANCE_PROTOCOL_INVALID", "broker returned a mismatched request_id"
            )
        if "error" in response:
            error = response["error"]
            raise MaintenanceError(error.get("code", "MAINTENANCE_REQUEST_FAILED"), error.get("message", ""))
        result = response.get("result")
        if not isinstance(result, dict):
            raise MaintenanceError(
                "MAINTENANCE_PROTOCOL_INVALID", "broker result must be a JSON object"
            )
        catalog_generation = result.get("catalog_generation")
        if isinstance(catalog_generation, int) and catalog_generation > 0:
            # Root may add/remove another installed source while this Product
            # keeps running. The broker accepts an older cached generation only
            # while this source's current secret still authenticates.
            self.catalog_generation = catalog_generation
        return result

    def _require_source(self) -> tuple[str, str]:
        if self.source_id is None or self.source_token is None:
            raise MaintenanceError("ACTIVITY_SOURCE_AUTH_REQUIRED", "source credentials are required")
        return self.source_id, self.source_token

    def _require_operator(self) -> str:
        if self.operator_token is None:
            raise MaintenanceError("OPERATOR_AUTH_REQUIRED", "operator capability is required")
        return self.operator_token

    def _catalog_generation(self, provided: int | None) -> int:
        generation = provided if provided is not None else self.catalog_generation
        if generation is None or generation <= 0:
            raise MaintenanceError(
                "UPDATE_READINESS_UNKNOWN", "installed catalog generation is required"
            )
        return generation


def _validate_plan_digest(value: str, field: str) -> None:
    """Requires the canonical full sha256 digest representation used on the wire."""
    if len(value) != 71 or not value.startswith("sha256:"):
        raise ValueError(f"{field} must use sha256:<64 lowercase hexadecimal characters>")
    if any(character not in "0123456789abcdef" for character in value[7:]):
        raise ValueError(f"{field} must use sha256:<64 lowercase hexadecimal characters>")
