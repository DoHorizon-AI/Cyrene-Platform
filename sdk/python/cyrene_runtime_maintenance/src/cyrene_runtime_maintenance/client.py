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
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Literal, Mapping

_BINDING_OPERATION_PROTOCOL_VERSION = "cyrene.runtime-maintenance.binding-operations.v1"
_SENSITIVE_ASSIGNMENT = re.compile(
    r"(?i)([\"']?(?:[a-z0-9_]*(?:token|password|secret|credential)|connection_ref)"
    r"[\"']?\s*:\s*[\"'])(.*?)([\"'])"
)
_SENSITIVE_INLINE = re.compile(
    r"(?i)(\b(?:[a-z0-9_]*token|password|secret|credential|connection_ref)\s*[=:]\s*)"
    r"(?:\"[^\"]*\"|'[^']*'|[^\s,;}]+)"
)
BindingOperation = Literal["activate", "recover", "deactivate"]


@dataclass(frozen=True, slots=True)
class BindingOperationScope:
    """The source-owned scope reserved for one package binding operation."""

    binding_id: str
    package_id: str
    installation_id: str
    operation: BindingOperation


@dataclass(frozen=True, slots=True, repr=False)
class BindingOperationReceipt:
    """Opaque broker lease receipt; the operation token is never shown in repr."""

    request_id: str
    source_id: str
    protocol_version: str
    catalog_generation: int
    scope: BindingOperationScope
    operation_token: str = field(repr=False)
    gate_generation: int
    already_in_flight: bool
    already_completed: bool

    def __repr__(self) -> str:
        """Hides the broker token and any receipt details from incidental logs."""
        return "BindingOperationReceipt(<redacted>)"


class MaintenanceError(RuntimeError):
    """A broker rejection or unavailable IPC endpoint with a stable code."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        readiness_status: str = "UNKNOWN",
        request_id: str | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.readiness_status = readiness_status
        self.request_id = request_id


class RuntimeMaintenanceClient:
    """Calls the authenticated local maintenance broker over a Unix socket.

    Args:
        socket_path: Broker socket path shared with the caller.
        source_id: Trusted catalog source ID for source-owned activity calls.
        source_token: Per-source token from a protected credential file.
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

    def reconcile_activity_source(self, active_tasks: Iterable[Mapping[str, str]]) -> dict[str, Any]:
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

    def _complete_binding_operation_after_owner_commit(
        self,
        receipt: BindingOperationReceipt,
        *,
        allow_in_flight: bool,
    ) -> dict[str, Any]:
        """Sends one exact-scope Broker completion after a Package Runtime owner commit."""
        source_id, source_token = self._require_source()
        if not isinstance(receipt, BindingOperationReceipt):
            raise TypeError("receipt must be a BindingOperationReceipt")
        if receipt.source_id != source_id:
            raise MaintenanceError("ACTIVITY_SOURCE_CALLER_MISMATCH", "receipt belongs to another source")
        if receipt.protocol_version != _BINDING_OPERATION_PROTOCOL_VERSION:
            raise MaintenanceError("PROTOCOL_VERSION_UNSUPPORTED", "binding operation protocol is unsupported")
        if self.catalog_generation is not None and self.catalog_generation != receipt.catalog_generation:
            raise MaintenanceError(
                "CATALOG_GENERATION_CHANGED", "receipt catalog generation does not match this source client"
            )
        if type(receipt.catalog_generation) is not int or receipt.catalog_generation <= 0:
            raise MaintenanceError("INVALID_ARGUMENT", "receipt catalog generation is invalid")
        if (
            not isinstance(receipt.scope, BindingOperationScope)
            or not isinstance(receipt.request_id, str)
            or not _is_safe_protocol_text(receipt.request_id, limit=256)
            or not isinstance(receipt.source_id, str)
            or not _is_safe_protocol_text(receipt.source_id, limit=256)
            or not isinstance(receipt.scope.binding_id, str)
            or not _is_safe_protocol_text(receipt.scope.binding_id)
            or not isinstance(receipt.scope.package_id, str)
            or not _is_safe_protocol_text(receipt.scope.package_id)
            or not isinstance(receipt.scope.installation_id, str)
            or not _is_safe_protocol_text(receipt.scope.installation_id)
            or not isinstance(receipt.scope.operation, str)
            or receipt.scope.operation not in {"activate", "recover", "deactivate"}
            or not isinstance(receipt.operation_token, str)
            or not _is_safe_protocol_text(receipt.operation_token, limit=4096)
            or type(receipt.gate_generation) is not int
            or receipt.gate_generation <= 0
            or type(receipt.already_in_flight) is not bool
            or type(receipt.already_completed) is not bool
            or (receipt.already_in_flight and receipt.already_completed)
        ):
            raise MaintenanceError("INVALID_ARGUMENT", "binding operation receipt is incomplete")
        if receipt.already_in_flight and not allow_in_flight:
            raise MaintenanceError(
                "BINDING_OPERATION_PENDING",
                "binding operation is still in flight",
                request_id=receipt.request_id,
            )

        try:
            result = self._request(
                "CompleteBindingOperation",
                {
                    "source_id": source_id,
                    "expected_catalog_generation": receipt.catalog_generation,
                    "binding_id": receipt.scope.binding_id,
                    "package_id": receipt.scope.package_id,
                    "installation_id": receipt.scope.installation_id,
                    "operation": receipt.scope.operation,
                    "operation_token": receipt.operation_token,
                },
                protocol_version=_BINDING_OPERATION_PROTOCOL_VERSION,
                request_id=receipt.request_id,
                redaction_secrets=(source_token, receipt.operation_token),
                strict_response=True,
            )
        except MaintenanceError as error:
            if error.request_id is None:
                error.request_id = receipt.request_id
            raise
        if type(result.get("completed")) is not bool or not result["completed"]:
            raise MaintenanceError(
                "MAINTENANCE_PROTOCOL_INVALID",
                "broker did not confirm binding completion",
                request_id=receipt.request_id,
            )
        if set(result) != {"completed", "gate_generation"}:
            raise MaintenanceError(
                "MAINTENANCE_PROTOCOL_INVALID",
                "broker completion result has an invalid shape",
                request_id=receipt.request_id,
            )
        gate_generation = result.get("gate_generation")
        if type(gate_generation) is not int or gate_generation <= 0:
            raise MaintenanceError(
                "MAINTENANCE_PROTOCOL_INVALID",
                "broker returned an invalid gate generation",
                request_id=receipt.request_id,
            )
        return result

    def _request(
        self,
        method: str,
        params: Mapping[str, Any],
        *,
        protocol_version: str | None = None,
        request_id: str | None = None,
        redaction_secrets: Iterable[str | None] = (),
        strict_response: bool = False,
    ) -> dict[str, Any]:
        """Sends one bounded JSON-line request and validates its matching response ID."""
        request_id = request_id or str(uuid.uuid4())
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
        if protocol_version is not None:
            request["protocol_version"] = protocol_version
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(self.timeout_seconds)
                connection.connect(self.socket_path)
                connection.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
                with connection.makefile("rb") as stream:
                    raw = stream.readline(1_048_577)
            if len(raw) > 1_048_576 or not raw.endswith(b"\n"):
                raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker response exceeded protocol bounds")
            response = json.loads(raw, object_pairs_hook=_unique_json_object)
        except MaintenanceError:
            raise
        except (OSError, json.JSONDecodeError, UnicodeDecodeError, ValueError) as error:
            raise MaintenanceError(
                "UPDATE_READINESS_UNKNOWN",
                f"runtime maintenance broker is unavailable: {error}",
                readiness_status="UNKNOWN",
            ) from error
        if strict_response and not isinstance(response, dict):
            raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker response must be an object")
        if strict_response and set(response) not in ({"request_id", "result"}, {"request_id", "error"}):
            raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker response has an invalid shape")
        if response.get("request_id") != request_id:
            raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker returned a mismatched request_id")
        if "error" in response:
            error = response["error"]
            if strict_response and not isinstance(error, dict):
                raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker error must be an object")
            if strict_response and not {"code", "message"}.issubset(error):
                raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker error fields are missing")
            code = error.get("code", "MAINTENANCE_REQUEST_FAILED")
            message = error.get("message", "")
            if strict_response and (
                not isinstance(code, str) or not _is_safe_protocol_text(code, limit=128) or not isinstance(message, str)
            ):
                raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker error fields are invalid")
            if not isinstance(code, str):
                code = "MAINTENANCE_REQUEST_FAILED"
            if not isinstance(message, str):
                message = "broker returned an invalid error message"
            raise MaintenanceError(
                _redact_broker_message(code, redaction_secrets),
                _redact_broker_message(message, redaction_secrets),
            )
        result = response.get("result")
        if not isinstance(result, dict):
            raise MaintenanceError("MAINTENANCE_PROTOCOL_INVALID", "broker result must be a JSON object")
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
            raise MaintenanceError("UPDATE_READINESS_UNKNOWN", "installed catalog generation is required")
        return generation


def _validate_plan_digest(value: str, field: str) -> None:
    """Requires the canonical full sha256 digest representation used on the wire."""
    if len(value) != 71 or not value.startswith("sha256:"):
        raise ValueError(f"{field} must use sha256:<64 lowercase hexadecimal characters>")
    if any(character not in "0123456789abcdef" for character in value[7:]):
        raise ValueError(f"{field} must use sha256:<64 lowercase hexadecimal characters>")


def _redact_broker_message(message: str, secrets: Iterable[str | None]) -> str:
    """Redacts known tokens and sensitive field values before surfacing errors."""
    safe = message
    for secret in sorted((value for value in secrets if value), key=len, reverse=True):
        safe = safe.replace(secret, "[REDACTED]")
    safe = _SENSITIVE_ASSIGNMENT.sub(r"\1[REDACTED]\3", safe)
    safe = _SENSITIVE_INLINE.sub(r"\1[REDACTED]", safe)
    safe = "".join(character for character in safe if character >= " " or character in "\t")
    return safe[:4096] + ("…" if len(safe) > 4096 else "")


def _is_safe_protocol_text(value: str, *, limit: int = 4096) -> bool:
    """Checks bounded protocol text without exposing malformed values in errors."""
    return bool(value.strip()) and len(value) <= limit and not any(ord(character) < 32 for character in value)


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Rejects ambiguous response objects that repeat a protocol field."""
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("JSON object contains a duplicate key")
        result[key] = value
    return result
