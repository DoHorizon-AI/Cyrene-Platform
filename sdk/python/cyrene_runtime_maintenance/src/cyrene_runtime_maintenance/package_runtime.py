"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 package_runtime.py                                               │
│  Module: cyrene_runtime_maintenance.package_runtime                  │
│  Role: Authenticated JSONL client for the Platform Package Runtime.  │
│                                                                      │
│  模块职责：通过独立 Unix socket 调用 Platform Package Runtime。       │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import inspect
import json
import math
import os
import re
import socket
import threading
import uuid
from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .client import BindingOperationReceipt, BindingOperationScope, MaintenanceError, RuntimeMaintenanceClient

DEFAULT_SOCKET_PATH = "/run/cyrene-package-runtime/control.sock"
_BROKER_SOCKET_PATH = "/run/cyrene/runtime-maintenance.sock"
_CONTROL_PROTOCOL_VERSION = "cy-package-runtime.control.v1"
_CONTROL_AUTHORITY = "platform_package_runtime"
_BINDING_OPERATION_CAPABILITY = "cy-package-runtime.binding-operation-admission.v1"
_BINDING_OPERATION_PROTOCOL_VERSION = "cyrene.runtime-maintenance.binding-operations.v1"
_STATUS_FIELDS = (
    "binding_id",
    "installation_id",
    "generation",
    "state",
    "failure_code",
    "failure_message",
    "connection_ref",
)
_MAX_CONTROL_LINE_BYTES = 1024 * 1024
_MAX_ERROR_TEXT_CHARS = 4096
_KNOWN_OPERATIONS = {
    "authority",
    "runtime_status",
    "get_installation",
    "activate",
    "recover_binding",
    "deactivate",
}
_BINDING_OPERATIONS = {"activate", "recover_binding", "deactivate"}
_SENSITIVE_ASSIGNMENT = re.compile(
    r"(?i)([\"']?(?:[a-z0-9_]*(?:token|password|secret|credential)|connection_ref)"
    r"[\"']?\s*:\s*[\"'])(.*?)([\"'])"
)
_SENSITIVE_INLINE = re.compile(
    r"(?i)(\b(?:[a-z0-9_]*token|password|secret|credential|connection_ref)\s*[=:]\s*)"
    r"(?:\"[^\"]*\"|'[^']*'|[^\s,;}]+)"
)


class PackageRuntimeError(RuntimeError):
    """A structured Package Runtime rejection or a local transport failure."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        remediation: str | None = None,
        binding_operation: BindingOperationReceipt | None = None,
        request_id: str | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.remediation = remediation
        self._binding_operation = binding_operation
        self.request_id = request_id

    @property
    def pending_binding_operation(self) -> BindingOperationReceipt | None:
        """Returns a validated pending receipt without including it in error text."""
        return self._binding_operation


@dataclass(frozen=True, slots=True, repr=False)
class PackageRuntimeOperationResult(Mapping[str, Any]):
    """RuntimeStatus mapping paired with a redacted, owner-completable receipt."""

    runtime_status: Mapping[str, Any] = field(repr=False)
    binding_operation: BindingOperationReceipt = field(repr=False)

    def __getitem__(self, key: str) -> Any:
        return self.runtime_status[key]

    def __iter__(self) -> Iterator[str]:
        return iter(self.runtime_status)

    def __len__(self) -> int:
        return len(self.runtime_status)

    def __repr__(self) -> str:
        """Avoids printing a connection reference or the Broker operation token."""
        return "PackageRuntimeOperationResult(<redacted>)"


@dataclass(frozen=True, slots=True, repr=False)
class PackageRuntimeReconciliationResult:
    """Read-only runtime/install snapshot reconciled against an owner receipt."""

    runtime_status: Mapping[str, Any] | None = field(repr=False)
    installation: Mapping[str, Any] = field(repr=False)
    binding_operation: BindingOperationReceipt = field(repr=False)

    def __repr__(self) -> str:
        """Avoids printing runtime references or the Broker operation token."""
        return "PackageRuntimeReconciliationResult(<redacted>)"


class PackageRuntimeClient:
    """Calls the local Package Runtime control socket using the current protocol.

    Cataloged workload callers authenticate with their installed source
    credential. This includes Product sources and separately cataloged
    standalone package operators; the daemon still checks the Unix peer
    credentials against the source policy. A root operator must opt into root
    mode explicitly. The source owner keeps lifecycle ownership and commits its
    durable state through the supplied callback. Control replies alone are not
    training or deployment proof.

    Args:
        socket_path: Absolute path to the Package Runtime UDS, separate from
            the runtime-maintenance broker socket.
        source_id: Installed catalog source ID for a Product or standalone owner.
        source_token: Secret loaded for that source from a protected file.
        operator: Explicit root-peer mode for an updater or other host operator.
        catalog_generation: Expected installed catalog generation, if known.
        timeout_seconds: Per-connection timeout for one request/response.
    """

    def __init__(
        self,
        socket_path: str | os.PathLike[str] = DEFAULT_SOCKET_PATH,
        *,
        source_id: str | None = None,
        source_token: str | None = None,
        operator: bool = False,
        catalog_generation: int | None = None,
        timeout_seconds: float = 5.0,
        maintenance_socket_path: str | os.PathLike[str] = "/run/cyrene/runtime-maintenance.sock",
        maintenance_client: RuntimeMaintenanceClient | None = None,
    ) -> None:
        path = os.fspath(socket_path)
        if not isinstance(path, str) or not path or "\x00" in path or not Path(path).is_absolute():
            raise ValueError("Package Runtime socket_path must be an absolute path")
        normalized_path = os.path.normpath("/" + path.lstrip("/"))
        if normalized_path == _BROKER_SOCKET_PATH:
            raise ValueError("Package Runtime requires its own control socket")
        if type(operator) is not bool:
            raise ValueError("operator must be a boolean")
        if not isinstance(timeout_seconds, (int, float)) or isinstance(timeout_seconds, bool):
            raise ValueError("timeout_seconds must be a positive finite number")
        if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be a positive finite number")
        if catalog_generation is not None and (type(catalog_generation) is not int or catalog_generation <= 0):
            raise ValueError("catalog_generation must be a positive integer")
        if (source_id is None) != (source_token is None):
            raise ValueError("source_id and source_token must be supplied together")
        if operator and source_id is not None:
            raise ValueError("operator mode cannot use source credentials")
        if not operator and source_id is None:
            raise ValueError("source credentials or explicit root operator mode are required")
        if source_id is not None and not isinstance(source_id, str):
            raise ValueError("source_id must be text")
        if source_token is not None and not isinstance(source_token, str):
            raise ValueError("source_token must be text")
        if source_id is not None and (not source_id.strip() or "\x00" in source_id):
            raise ValueError("source_id must be non-empty")
        normalized_token = source_token.strip() if source_token is not None else None
        if source_token is not None and not normalized_token:
            raise ValueError("source_token must be non-empty")
        if operator and getattr(os, "geteuid", lambda: -1)() != 0:
            raise ValueError("operator mode requires a root process")
        maintenance_path = os.fspath(maintenance_socket_path)
        if not isinstance(maintenance_path, str) or not maintenance_path or not Path(maintenance_path).is_absolute():
            raise ValueError("maintenance_socket_path must be an absolute path")
        if maintenance_client is not None:
            if operator or source_id is None or normalized_token is None:
                raise ValueError("a maintenance client requires source credentials")
            if (
                maintenance_client.source_id != source_id.strip()
                or maintenance_client.source_token != normalized_token
                or maintenance_client.operator_token is not None
            ):
                raise ValueError("maintenance client must use the same source credentials")

        self.socket_path = path
        self.source_id = source_id.strip() if source_id is not None else None
        self._source_token = normalized_token
        self.operator = operator
        self._catalog_generation = catalog_generation
        self.timeout_seconds = float(timeout_seconds)
        self._authority_checked = False
        self._authority_lock = threading.Lock()
        self.maintenance_socket_path = maintenance_path
        self._maintenance_client = maintenance_client
        self._authority_capabilities: frozenset[str] = frozenset()

    @classmethod
    def from_source_secret(
        cls,
        source_id: str,
        token_path: str | os.PathLike[str],
        *,
        socket_path: str | os.PathLike[str] = DEFAULT_SOCKET_PATH,
        catalog_generation: int | None = None,
        timeout_seconds: float = 5.0,
        maintenance_socket_path: str | os.PathLike[str] = "/run/cyrene/runtime-maintenance.sock",
    ) -> PackageRuntimeClient:
        """Loads one cataloged source token from a protected file without logging it.

        The caller must run with the UID/GID assigned to ``source_id`` in the
        installed Package Runtime source policy. The token file path is an
        installer/service credential setting, not a Package Runtime policy field.
        """
        token = _read_token_file(token_path)
        return cls(
            socket_path,
            source_id=source_id,
            source_token=token,
            catalog_generation=catalog_generation,
            timeout_seconds=timeout_seconds,
            maintenance_socket_path=maintenance_socket_path,
        )

    @classmethod
    def from_environment(cls) -> PackageRuntimeClient:
        """Loads the current Product source from installer/systemd settings.

        The existing activity-source ID and token-file setting are reused. When
        systemd credentials are present, ``activity-token`` is read from
        ``CREDENTIALS_DIRECTORY``. The environment is read only and never
        modified by this module.
        """
        source_id = os.environ.get("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID", "").strip()
        if not source_id:
            raise ValueError("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID is required")

        token_path = os.environ.get("CYRENE_RUNTIME_ACTIVITY_SOURCE_TOKEN_FILE", "").strip()
        if not token_path:
            credentials_directory = os.environ.get("CREDENTIALS_DIRECTORY", "").strip()
            token_path = (
                str(Path(credentials_directory) / "activity-token")
                if credentials_directory
                else "/run/secrets/cyrene-runtime-activity-token"
            )

        generation_text = os.environ.get("CYRENE_RUNTIME_ACTIVITY_CATALOG_GENERATION", "").strip()
        try:
            catalog_generation = int(generation_text) if generation_text else None
        except ValueError as error:
            raise ValueError("CYRENE_RUNTIME_ACTIVITY_CATALOG_GENERATION must be a positive integer") from error

        socket_path = os.environ.get("CYRENE_PACKAGE_RUNTIME_SOCKET", DEFAULT_SOCKET_PATH)
        maintenance_socket_path = os.environ.get(
            "CYRENE_RUNTIME_MAINTENANCE_SOCKET", "/run/cyrene/runtime-maintenance.sock"
        )
        return cls.from_source_secret(
            source_id,
            token_path,
            socket_path=socket_path,
            catalog_generation=catalog_generation,
            maintenance_socket_path=maintenance_socket_path,
        )

    @property
    def catalog_generation(self) -> int | None:
        """Returns the generation accepted by the last successful Authority check."""
        return self._catalog_generation

    def authority(self) -> dict[str, Any]:
        """Checks daemon identity and refreshes the read-only generation handshake.

        An unset generation is learned from Authority. If the daemon reports a
        different generation later, the check fails and the old value is kept.
        中文：目录代次变化时保持旧值并拒绝继续调用，需由调用方重新装载凭证与配置。
        """
        self._authority_checked = False
        self._authority_capabilities = frozenset()
        request = self._make_request("authority", {}, include_generation=False)
        result = self._exchange(request, secrets=_secret_values(self._source_token, {}))
        self._validate_authority(result)
        return result

    def runtime_status(self, binding_id: str) -> dict[str, Any]:
        """Returns the daemon's status for one source-owned binding."""
        return self._request("runtime_status", {"binding_id": _require_text(binding_id, "binding_id")})

    def get_installation(self, installation_id: str) -> dict[str, Any]:
        """Returns the installed record identified by ``installation_id``."""
        return self._request("get_installation", {"installation_id": _require_text(installation_id, "installation_id")})

    def activate(
        self,
        binding_id: str,
        installation_id: str,
        *,
        package_id: str,
        environment: Mapping[str, str] | None = None,
        request_id: str | None = None,
    ) -> PackageRuntimeOperationResult:
        """Activates one binding; reuse ``request_id`` only for explicit replay.

        The returned receipt must be completed only after the owner durably
        commits its binding state. Prefer ``activate_and_persist`` so owner
        intent is durable before dispatch. No mutation is retried automatically.
        """
        return self._binding_operation(
            "activate",
            {
                "binding_id": _require_text(binding_id, "binding_id"),
                "installation_id": _require_text(installation_id, "installation_id"),
                "environment": _copy_environment(environment),
            },
            expected_scope=BindingOperationScope(
                _require_text(binding_id, "binding_id"),
                _require_text(package_id, "package_id"),
                _require_text(installation_id, "installation_id"),
                "activate",
            ),
            request_id=request_id,
        )

    def recover_binding(
        self,
        binding_id: str,
        *,
        package_id: str,
        installation_id: str,
        environment: Mapping[str, str] | None = None,
        request_id: str | None = None,
    ) -> PackageRuntimeOperationResult:
        """Recovers one binding; reuse ``request_id`` only for explicit replay."""
        return self._binding_operation(
            "recover_binding",
            {
                "binding_id": _require_text(binding_id, "binding_id"),
                "environment": _copy_environment(environment),
            },
            expected_scope=BindingOperationScope(
                _require_text(binding_id, "binding_id"),
                _require_text(package_id, "package_id"),
                _require_text(installation_id, "installation_id"),
                "recover",
            ),
            request_id=request_id,
        )

    def deactivate(
        self,
        binding_id: str,
        *,
        package_id: str,
        installation_id: str,
        request_id: str | None = None,
    ) -> PackageRuntimeOperationResult:
        """Deactivates one binding; reuse ``request_id`` only for explicit replay."""
        return self._binding_operation(
            "deactivate",
            {"binding_id": _require_text(binding_id, "binding_id")},
            expected_scope=BindingOperationScope(
                _require_text(binding_id, "binding_id"),
                _require_text(package_id, "package_id"),
                _require_text(installation_id, "installation_id"),
                "deactivate",
            ),
            request_id=request_id,
        )

    def activate_and_persist(
        self,
        binding_id: str,
        installation_id: str,
        *,
        package_id: str,
        persist_intent: Callable[[str, BindingOperationScope], None],
        persist: Callable[[Mapping[str, Any], BindingOperationReceipt], None],
        request_id: str | None = None,
        environment: Mapping[str, str] | None = None,
    ) -> dict[str, Any]:
        """Persists operation intent, activates, persists status, then completes the lease.

        ``persist_intent`` must durably bind the request ID to this exact scope
        before any network request. Reuse that ID only for an explicit replay;
        the intent callback must verify an existing ID has the same scope.
        ``persist`` must synchronously commit the resulting owner record and
        connection reference, return ``None``, and raise on failure.
        """
        binding_id = _require_text(binding_id, "binding_id")
        installation_id = _require_text(installation_id, "installation_id")
        package_id = _require_text(package_id, "package_id")
        environment = _copy_environment(environment)
        _require_callback(persist)
        scope = BindingOperationScope(binding_id, package_id, installation_id, "activate")
        request_id = self._persist_operation_intent(request_id, scope, persist_intent)
        result = self.activate(
            binding_id,
            installation_id,
            package_id=package_id,
            environment=environment,
            request_id=request_id,
        )
        return self._persist_and_complete(result, persist)

    def recover_and_persist(
        self,
        binding_id: str,
        *,
        package_id: str,
        installation_id: str,
        persist_intent: Callable[[str, BindingOperationScope], None],
        persist: Callable[[Mapping[str, Any], BindingOperationReceipt], None],
        request_id: str | None = None,
        environment: Mapping[str, str] | None = None,
    ) -> dict[str, Any]:
        """Persists intent, recovers a binding, commits owner state, then completes."""
        binding_id = _require_text(binding_id, "binding_id")
        package_id = _require_text(package_id, "package_id")
        installation_id = _require_text(installation_id, "installation_id")
        environment = _copy_environment(environment)
        _require_callback(persist)
        scope = BindingOperationScope(binding_id, package_id, installation_id, "recover")
        request_id = self._persist_operation_intent(request_id, scope, persist_intent)
        result = self.recover_binding(
            binding_id,
            package_id=package_id,
            installation_id=installation_id,
            environment=environment,
            request_id=request_id,
        )
        return self._persist_and_complete(result, persist)

    def deactivate_and_persist(
        self,
        binding_id: str,
        *,
        package_id: str,
        installation_id: str,
        persist_intent: Callable[[str, BindingOperationScope], None],
        persist: Callable[[Mapping[str, Any], BindingOperationReceipt], None],
        request_id: str | None = None,
    ) -> dict[str, Any]:
        """Persists intent, deactivates, commits owner state, then completes the lease."""
        binding_id = _require_text(binding_id, "binding_id")
        package_id = _require_text(package_id, "package_id")
        installation_id = _require_text(installation_id, "installation_id")
        _require_callback(persist)
        scope = BindingOperationScope(binding_id, package_id, installation_id, "deactivate")
        request_id = self._persist_operation_intent(request_id, scope, persist_intent)
        result = self.deactivate(
            binding_id,
            package_id=package_id,
            installation_id=installation_id,
            request_id=request_id,
        )
        return self._persist_and_complete(result, persist)

    def _complete_binding_operation_after_owner_commit(
        self,
        receipt: BindingOperationReceipt,
        *,
        allow_in_flight: bool,
    ) -> dict[str, Any]:
        """Completes a receipt only after an owner-controlled durable commit."""
        self._require_source_credentials()
        if not isinstance(receipt, BindingOperationReceipt):
            raise TypeError("receipt must be a BindingOperationReceipt")
        if receipt.source_id != self.source_id:
            raise PackageRuntimeError("ACTIVITY_SOURCE_CALLER_MISMATCH", "receipt belongs to another source")
        if self._catalog_generation is None or not self._authority_checked:
            self.authority()
        self._require_binding_operation_capability()
        if receipt.catalog_generation != self._catalog_generation:
            raise PackageRuntimeError(
                "CATALOG_GENERATION_CHANGED", "receipt catalog generation does not match Package Runtime"
            )
        return self._get_maintenance_client(receipt.catalog_generation)._complete_binding_operation_after_owner_commit(
            receipt,
            allow_in_flight=allow_in_flight,
        )

    def reconcile_binding_operation_and_persist(
        self,
        receipt: BindingOperationReceipt,
        *,
        reconcile: Callable[[Mapping[str, Any] | None, Mapping[str, Any], BindingOperationReceipt], None],
    ) -> PackageRuntimeReconciliationResult:
        """Reconciles a pending lease from read-only evidence, then completes it.

        The owner callback must validate its durable intent and operation-specific
        expected state, commit the observed outcome, and return ``None``. This
        method never changes receipt flags or repeats the original mutation.
        """
        self._require_source_credentials()
        if not isinstance(receipt, BindingOperationReceipt):
            raise TypeError("receipt must be a BindingOperationReceipt")
        if not callable(reconcile):
            raise TypeError("reconcile must be callable")

        scope = receipt.scope
        try:
            validated_receipt = _parse_binding_operation_receipt(
                _binding_operation_receipt_value(receipt),
                request_id=receipt.request_id,
                source_id=self.source_id,
                expected_generation=receipt.catalog_generation,
                expected_scope=scope,
            )
            if validated_receipt != receipt:
                raise PackageRuntimeError(
                    "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt is not canonical"
                )
            if receipt.already_completed:
                raise PackageRuntimeError(
                    "BINDING_OPERATION_NOT_PENDING", "a completed binding receipt cannot be reconciled"
                )

            # A fresh handshake confirms this source still uses the same catalog
            # generation and that the daemon still exposes the admission bridge.
            authority = self.authority()
            if authority["catalog_generation"] != receipt.catalog_generation:
                raise PackageRuntimeError(
                    "CATALOG_GENERATION_CHANGED", "receipt catalog generation does not match Package Runtime"
                )
            self._require_binding_operation_capability()

            try:
                status_value = self.runtime_status(scope.binding_id)
            except PackageRuntimeError as error:
                if scope.operation != "activate" or error.code != "BINDING_NOT_FOUND":
                    raise
                # A first activation may fail before it writes a binding record.
                # runtime_status is lifecycle-serialized, so this absence is
                # observed after the original synchronous mutation has ended.
                status = None
            else:
                status = (
                    _validate_observed_runtime_status(status_value, scope.binding_id)
                    if scope.operation == "activate"
                    else _validate_runtime_status(status_value, scope)
                )
            installation = _validate_installation_scope(self.get_installation(scope.installation_id), scope)

            try:
                callback_result = reconcile(status, installation, receipt)
            except Exception:
                raise PackageRuntimeError(
                    "OWNER_RECONCILIATION_FAILED",
                    "owner reconciliation callback failed; the binding operation remains pending",
                    request_id=receipt.request_id,
                ) from None
            if inspect.isawaitable(callback_result):
                close = getattr(callback_result, "close", None)
                if callable(close):
                    close()
                raise PackageRuntimeError(
                    "OWNER_RECONCILIATION_INCOMPLETE",
                    "reconcile callback must finish synchronously; the binding operation remains pending",
                    request_id=receipt.request_id,
                )
            if callback_result is not None:
                raise PackageRuntimeError(
                    "OWNER_RECONCILIATION_INCOMPLETE",
                    "reconcile callback must return None after its durable commit; the binding operation remains pending",
                    request_id=receipt.request_id,
                )

            self._complete_binding_operation_after_owner_commit(receipt, allow_in_flight=True)
            return PackageRuntimeReconciliationResult(status, installation, receipt)
        except PackageRuntimeError as error:
            if error.request_id is None:
                error.request_id = receipt.request_id
            raise
        except MaintenanceError as error:
            error.request_id = receipt.request_id
            raise

    def commit_binding_operation_outcome(
        self,
        receipt: BindingOperationReceipt,
        *,
        persist_outcome: Callable[[BindingOperationReceipt], None],
    ) -> None:
        """Persists an owner-observed outcome, then completes its pending lease.

        Use this for an explicit failure outcome when no runtime status exists
        to reconcile, such as the first activation of a binding failing before
        its activation record is created. The callback must durably commit the
        owner's outcome and be safe to replay with the same receipt. A callback
        failure leaves the Broker lease pending. This method never retries the
        Package Runtime mutation.
        """
        self._require_source_credentials()
        if not isinstance(receipt, BindingOperationReceipt):
            raise TypeError("receipt must be a BindingOperationReceipt")
        if not callable(persist_outcome):
            raise TypeError("persist_outcome must be callable")
        if receipt.source_id != self.source_id:
            raise PackageRuntimeError("ACTIVITY_SOURCE_CALLER_MISMATCH", "receipt belongs to another source")
        if receipt.already_completed:
            raise PackageRuntimeError(
                "BINDING_OPERATION_NOT_PENDING", "a completed binding receipt cannot be committed again"
            )
        if receipt.already_in_flight:
            raise PackageRuntimeError(
                "BINDING_OPERATION_PENDING",
                "an in-flight lease must be reconciled from runtime readback before it can be completed",
                request_id=receipt.request_id,
            )

        scope = receipt.scope
        validated_receipt = _parse_binding_operation_receipt(
            _binding_operation_receipt_value(receipt),
            request_id=receipt.request_id,
            source_id=self.source_id,
            expected_generation=receipt.catalog_generation,
            expected_scope=scope,
        )
        if validated_receipt != receipt:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt is not canonical"
            )

        authority = self.authority()
        if authority["catalog_generation"] != receipt.catalog_generation:
            raise PackageRuntimeError(
                "CATALOG_GENERATION_CHANGED", "receipt catalog generation does not match Package Runtime"
            )
        self._require_binding_operation_capability()
        _invoke_sync_none_callback(
            persist_outcome,
            (receipt,),
            "persist_outcome",
            "persist_outcome callback must return None after its durable commit",
        )
        self._complete_binding_operation_after_owner_commit(
            receipt,
            allow_in_flight=False,
        )

    def _binding_operation(
        self,
        operation: str,
        fields: Mapping[str, Any],
        *,
        expected_scope: BindingOperationScope,
        request_id: str | None = None,
    ) -> PackageRuntimeOperationResult:
        """Sends one source-owned mutation and validates its daemon-issued receipt."""
        request_id = _resolve_request_id(request_id)
        try:
            self._require_source_credentials()
            result, sent_request_id = self._request_with_id(
                operation, fields, expected_scope=expected_scope, request_id=request_id
            )
            status = _validate_runtime_status(result, expected_scope)
            receipt = _parse_binding_operation_receipt(
                result.get("binding_operation"),
                request_id=sent_request_id,
                source_id=self.source_id,
                expected_generation=self._catalog_generation,
                expected_scope=expected_scope,
            )
            if receipt.already_in_flight:
                raise PackageRuntimeError(
                    "BINDING_OPERATION_PENDING",
                    "a matching binding operation is still pending",
                    binding_operation=receipt,
                    request_id=request_id,
                )
            return PackageRuntimeOperationResult(status, receipt)
        except PackageRuntimeError as error:
            if error.request_id is None:
                error.request_id = request_id
            raise

    def _persist_and_complete(
        self,
        result: PackageRuntimeOperationResult,
        persist: Callable[[Mapping[str, Any], BindingOperationReceipt], None],
    ) -> dict[str, Any]:
        """Leaves the gate pending on callback or Broker completion failure."""
        _invoke_sync_none_callback(
            persist,
            (result.runtime_status, result.binding_operation),
            "persist",
            "persist callback must return None after its durable commit",
        )
        self._complete_binding_operation_after_owner_commit(result.binding_operation, allow_in_flight=False)
        return dict(result.runtime_status)

    def _persist_operation_intent(
        self,
        request_id: str | None,
        scope: BindingOperationScope,
        persist_intent: Callable[[str, BindingOperationScope], None],
    ) -> str:
        """Durably records a stable request ID and scope before any socket call."""
        request_id = _resolve_request_id(request_id)
        if not callable(persist_intent):
            raise TypeError("persist_intent must be callable")
        _invoke_sync_none_callback(
            persist_intent,
            (request_id, scope),
            "persist_intent",
            "persist_intent callback must return None after its durable commit",
        )
        return request_id

    def _get_maintenance_client(self, generation: int) -> RuntimeMaintenanceClient:
        if self._maintenance_client is None:
            self._maintenance_client = RuntimeMaintenanceClient(
                self.maintenance_socket_path,
                source_id=self.source_id,
                source_token=self._source_token,
                catalog_generation=generation,
                timeout_seconds=self.timeout_seconds,
            )
        return self._maintenance_client

    def _require_source_credentials(self) -> None:
        if self.operator or self.source_id is None or self._source_token is None:
            raise PackageRuntimeError(
                "ACTIVITY_SOURCE_AUTH_REQUIRED", "catalog source credentials are required for binding operations"
            )

    def _require_binding_operation_capability(self) -> None:
        if _BINDING_OPERATION_CAPABILITY not in self._authority_capabilities:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_CAPABILITY_REQUIRED",
                "Package Runtime does not advertise binding operation admission",
            )

    def _request(self, operation: str, fields: Mapping[str, Any]) -> dict[str, Any]:
        """Checks authority once, then sends exactly one non-retried operation."""
        return self._request_with_id(operation, fields)[0]

    def _request_with_id(
        self,
        operation: str,
        fields: Mapping[str, Any],
        *,
        expected_scope: BindingOperationScope | None = None,
        request_id: str | None = None,
    ) -> tuple[dict[str, Any], str]:
        """Sends one operation and returns its result with the exact wire request ID."""
        if operation not in _KNOWN_OPERATIONS or operation == "authority":
            raise ValueError("operation is not exposed by this client")
        request_id = _resolve_request_id(request_id)
        try:
            self._ensure_authority()
            if operation in _BINDING_OPERATIONS:
                if expected_scope is None:
                    raise ValueError("binding operation scope is required")
                self._require_binding_operation_capability()
            request = self._make_request(operation, fields, include_generation=True, request_id=request_id)
            result = self._exchange(
                request,
                secrets=_secret_values(self._source_token, fields),
                expected_scope=expected_scope,
            )
            return result, request["request_id"]
        except PackageRuntimeError as error:
            if error.request_id is None:
                error.request_id = request_id
            raise

    def _ensure_authority(self) -> None:
        with self._authority_lock:
            if not self._authority_checked:
                self.authority()

    def _make_request(
        self,
        operation: str,
        fields: Mapping[str, Any],
        *,
        include_generation: bool,
        request_id: str | None = None,
    ) -> dict[str, Any]:
        request: dict[str, Any] = {
            "request_id": _resolve_request_id(request_id),
            "operation": operation,
            **fields,
            "auth": self._auth_payload(),
        }
        if include_generation:
            if self._catalog_generation is None:
                raise PackageRuntimeError(
                    "PACKAGE_RUNTIME_PROTOCOL_INVALID",
                    "Authority did not establish a catalog generation",
                )
            request["catalog_generation"] = self._catalog_generation
        return request

    def _auth_payload(self) -> dict[str, str]:
        if self.operator:
            # The daemon authorizes this empty credential object only for peer UID 0.
            return {}
        if self.source_id is None or self._source_token is None:
            raise PackageRuntimeError("SOURCE_AUTH_REQUIRED", "catalog source credentials are required")
        return {"source_id": self.source_id, "source_token": self._source_token}

    def _exchange(
        self,
        request: Mapping[str, Any],
        *,
        secrets: tuple[str, ...],
        expected_scope: BindingOperationScope | None = None,
    ) -> dict[str, Any]:
        """Sends one bounded JSONL message and checks its correlated response."""
        request_id = request.get("request_id")
        try:
            encoded = json.dumps(request, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        except (TypeError, ValueError) as error:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_REQUEST_INVALID", "Package Runtime request is not JSON serializable"
            ) from error
        if len(encoded) + 1 > _MAX_CONTROL_LINE_BYTES:
            raise PackageRuntimeError("CONTROL_REQUEST_TOO_LARGE", "Package Runtime request exceeds the one MiB limit")

        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(self.timeout_seconds)
                connection.connect(self.socket_path)
                connection.sendall(encoded + b"\n")
                with connection.makefile("rb") as stream:
                    raw = stream.readline(_MAX_CONTROL_LINE_BYTES + 1)
        except (socket.timeout, TimeoutError) as error:
            raise PackageRuntimeError("CONTROL_TIMEOUT", "Package Runtime control request timed out") from error
        except OSError as error:
            raise PackageRuntimeError("CONTROL_UNAVAILABLE", "Package Runtime control socket is unavailable") from error

        if len(raw) > _MAX_CONTROL_LINE_BYTES:
            raise PackageRuntimeError(
                "CONTROL_RESPONSE_TOO_LARGE", "Package Runtime response exceeds the one MiB limit"
            )
        if not raw.endswith(b"\n"):
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime returned an incomplete JSONL response"
            )
        try:
            response = json.loads(raw[:-1], object_pairs_hook=_unique_json_object)
        except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as error:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime returned malformed JSON"
            ) from error

        if not isinstance(response, dict):
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime response must be a JSON object"
            )
        if response.get("request_id") != request_id:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime returned a mismatched request_id"
            )
        ok = response.get("ok")
        if type(ok) is not bool:
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime response ok must be a boolean"
            )
        if ok:
            if set(response) != {"request_id", "ok", "result"} or not isinstance(response.get("result"), dict):
                raise PackageRuntimeError(
                    "PACKAGE_RUNTIME_PROTOCOL_INVALID",
                    "successful Package Runtime response must contain one result object",
                )
            return response["result"]

        error_value = response.get("error")
        if set(response) != {"request_id", "ok", "error"} or not isinstance(error_value, dict):
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID",
                "failed Package Runtime response must contain one structured error",
            )
        code = error_value.get("code")
        message = error_value.get("message")
        remediation = error_value.get("remediation")
        if (
            not {"code", "message", "remediation"}.issubset(error_value)
            or set(error_value) - {"code", "message", "remediation", "binding_operation"}
            or not isinstance(code, str)
            or not code.strip()
            or any(ord(character) < 32 for character in code)
            or not isinstance(message, str)
            or not isinstance(remediation, str)
        ):
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Package Runtime error fields have invalid types"
            )
        operation_receipt = None
        if "binding_operation" in error_value:
            if expected_scope is None:
                raise PackageRuntimeError(
                    "PACKAGE_RUNTIME_PROTOCOL_INVALID",
                    "Package Runtime attached a binding receipt to a non-binding error",
                )
            operation_receipt = _parse_binding_operation_receipt(
                error_value["binding_operation"],
                request_id=request_id,
                source_id=self.source_id,
                expected_generation=self._catalog_generation,
                expected_scope=expected_scope,
            )
        safe_code = _redact(code, secrets)
        receipt_secrets = (operation_receipt.operation_token,) if operation_receipt is not None else ()
        raise PackageRuntimeError(
            _redact(safe_code, receipt_secrets),
            _redact(message, (*secrets, *receipt_secrets)),
            remediation=_redact(remediation, (*secrets, *receipt_secrets)),
            binding_operation=operation_receipt,
        )

    def _validate_authority(self, result: dict[str, Any]) -> None:
        """Checks the exact server identity and fails closed on generation drift."""
        if result.get("authority") != _CONTROL_AUTHORITY:
            self._authority_checked = False
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_AUTHORITY_MISMATCH", "socket is not the Platform Package Runtime"
            )
        if result.get("protocol_version") != _CONTROL_PROTOCOL_VERSION:
            self._authority_checked = False
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_MISMATCH", "Package Runtime control protocol is unsupported"
            )
        generation = result.get("catalog_generation")
        if type(generation) is not int or generation <= 0:
            self._authority_checked = False
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID",
                "Authority must return a positive catalog_generation",
            )
        capabilities = result.get("capabilities", [])
        if not isinstance(capabilities, list) or any(
            not isinstance(capability, str) or not capability for capability in capabilities
        ):
            self._authority_checked = False
            raise PackageRuntimeError(
                "PACKAGE_RUNTIME_PROTOCOL_INVALID", "Authority capabilities must be a list of non-empty strings"
            )
        if self._catalog_generation is not None and generation != self._catalog_generation:
            self._authority_checked = False
            raise PackageRuntimeError(
                "CATALOG_GENERATION_CHANGED",
                "Package Runtime catalog generation changed; reload current credentials and settings",
            )
        self._catalog_generation = generation
        self._authority_capabilities = frozenset(capabilities)
        self._authority_checked = True


def _read_token_file(token_path: str | os.PathLike[str]) -> str:
    """Reads a protected source token and returns only its stripped contents."""
    try:
        token = Path(token_path).read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError):
        raise ValueError("could not read source token credential") from None
    if not token:
        raise ValueError("source token credential is empty")
    return token


def _require_callback(callback: Callable[..., Any]) -> None:
    """Rejects a missing/non-callable owner callback before mutation or I/O."""
    if not callable(callback):
        raise TypeError("persist must be callable")


def _resolve_request_id(request_id: str | None) -> str:
    """Returns a bounded caller-stable request ID or creates one before I/O."""
    if request_id is None:
        return str(uuid.uuid4())
    if not _is_protocol_text(request_id, max_length=256):
        raise ValueError("request_id must be non-empty text of at most 256 characters without controls")
    try:
        encoded = request_id.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError("request_id must be valid UTF-8 text") from None
    if len(encoded) > 256:
        raise ValueError("request_id must be at most 256 UTF-8 bytes")
    return request_id


def _invoke_sync_none_callback(
    callback: Callable[..., Any],
    args: tuple[Any, ...],
    name: str,
    return_error: str,
) -> None:
    """Runs one owner callback and rejects async or non-None completion."""
    result = callback(*args)
    if inspect.isawaitable(result):
        close = getattr(result, "close", None)
        if callable(close):
            close()
        raise TypeError(f"{name} callback must finish synchronously")
    if result is not None:
        raise TypeError(return_error)


def _binding_operation_receipt_value(receipt: BindingOperationReceipt) -> dict[str, Any]:
    """Creates the strict wire-shaped view used only to revalidate a typed receipt."""
    if not isinstance(receipt.scope, BindingOperationScope):
        raise PackageRuntimeError("PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt scope is invalid")
    return {
        "request_id": receipt.request_id,
        "source_id": receipt.source_id,
        "protocol_version": receipt.protocol_version,
        "scope": {
            "binding_id": receipt.scope.binding_id,
            "package_id": receipt.scope.package_id,
            "installation_id": receipt.scope.installation_id,
            "operation": receipt.scope.operation,
        },
        "catalog_generation": receipt.catalog_generation,
        "gate_generation": receipt.gate_generation,
        "operation_token": receipt.operation_token,
        "already_in_flight": receipt.already_in_flight,
        "already_completed": receipt.already_completed,
    }


def _require_text(value: str, field: str) -> str:
    """Rejects empty operation identifiers before opening the control socket."""
    if not isinstance(value, str) or not value.strip() or "\x00" in value:
        raise ValueError(f"{field} must be non-empty text")
    return value


def _validate_installation_scope(
    value: Any,
    expected_scope: BindingOperationScope,
) -> dict[str, Any]:
    """Requires the read-back installation to match the pending binding lease."""
    if not isinstance(value, dict):
        raise PackageRuntimeError("PACKAGE_RUNTIME_PROTOCOL_INVALID", "installation read-back must be an object")
    installation_id = value.get("installation_id")
    package_id = value.get("package_id")
    state = value.get("state")
    if (
        not _is_protocol_text(installation_id)
        or installation_id != expected_scope.installation_id
        or not _is_protocol_text(package_id)
        or package_id != expected_scope.package_id
        or state != "INSTALLED"
    ):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_SCOPE_MISMATCH",
            "installation read-back does not match the pending binding operation",
        )
    return dict(value)


def _copy_environment(environment: Mapping[str, str] | None) -> dict[str, str]:
    """Copies validated environment values without mutating caller state."""
    if environment is None:
        return {}
    if not isinstance(environment, Mapping):
        raise ValueError("environment must be a string mapping")
    copied: dict[str, str] = {}
    for key, value in environment.items():
        if not isinstance(key, str) or not key or "\x00" in key:
            raise ValueError("environment keys must be non-empty text")
        if not isinstance(value, str) or "\x00" in value:
            raise ValueError("environment values must be text")
        copied[key] = value
    return copied


def _parse_binding_operation_receipt(
    value: Any,
    *,
    request_id: Any,
    source_id: str | None,
    expected_generation: int | None,
    expected_scope: BindingOperationScope,
) -> BindingOperationReceipt:
    """Validates every lease field against this request and source scope.

    A receipt is the only proof that the daemon reserved this exact operation.
    Its opaque token stays out of all error text and representations.
    """
    fields = {
        "request_id",
        "source_id",
        "protocol_version",
        "scope",
        "catalog_generation",
        "gate_generation",
        "operation_token",
        "already_in_flight",
        "already_completed",
    }
    if not isinstance(value, dict) or set(value) != fields:
        raise PackageRuntimeError("PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt has an invalid shape")

    receipt_request_id = value["request_id"]
    receipt_source_id = value["source_id"]
    protocol_version = value["protocol_version"]
    generation = value["catalog_generation"]
    gate_generation = value["gate_generation"]
    operation_token = value["operation_token"]
    in_flight = value["already_in_flight"]
    completed = value["already_completed"]
    scope_value = value["scope"]
    if (
        not isinstance(receipt_request_id, str)
        or not receipt_request_id
        or len(receipt_request_id) > 256
        or any(ord(character) < 32 for character in receipt_request_id)
        or receipt_request_id != request_id
        or not isinstance(receipt_source_id, str)
        or not receipt_source_id.strip()
        or any(ord(character) < 32 for character in receipt_source_id)
        or source_id is None
        or receipt_source_id != source_id
        or protocol_version != _BINDING_OPERATION_PROTOCOL_VERSION
        or type(generation) is not int
        or generation <= 0
        or expected_generation is None
        or generation != expected_generation
        or type(gate_generation) is not int
        or gate_generation <= 0
        or not isinstance(operation_token, str)
        or not operation_token
        or len(operation_token) > 4096
        or any(ord(character) < 32 for character in operation_token)
        or type(in_flight) is not bool
        or type(completed) is not bool
        or (in_flight and completed)
    ):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt fields do not match this request"
        )

    scope_fields = {"binding_id", "package_id", "installation_id", "operation"}
    if not isinstance(scope_value, dict) or set(scope_value) != scope_fields:
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt scope has an invalid shape"
        )
    binding_id, package_id, installation_id, operation = (
        scope_value["binding_id"],
        scope_value["package_id"],
        scope_value["installation_id"],
        scope_value["operation"],
    )
    if (
        not _is_protocol_text(binding_id)
        or not _is_protocol_text(package_id)
        or not _is_protocol_text(installation_id)
        or not isinstance(operation, str)
        or operation not in {"activate", "recover", "deactivate"}
    ):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt scope fields are invalid"
        )
    scope = BindingOperationScope(binding_id, package_id, installation_id, operation)
    if scope != expected_scope:
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation receipt scope does not match this request"
        )

    return BindingOperationReceipt(
        request_id=receipt_request_id,
        source_id=receipt_source_id,
        protocol_version=protocol_version,
        catalog_generation=generation,
        scope=scope,
        operation_token=operation_token,
        gate_generation=gate_generation,
        already_in_flight=in_flight,
        already_completed=completed,
    )


def _validate_runtime_status(
    value: Any,
    expected_scope: BindingOperationScope,
) -> dict[str, Any]:
    """Validates the unmodified RuntimeStatus alongside an admitted mutation."""
    if not isinstance(value, dict) or not set(_STATUS_FIELDS).issubset(value):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation returned an invalid RuntimeStatus"
        )
    if (
        value["binding_id"] != expected_scope.binding_id
        or value["installation_id"] != expected_scope.installation_id
        or not _is_protocol_text(value["binding_id"])
        or not _is_protocol_text(value["installation_id"])
        or type(value["generation"]) is not int
        or value["generation"] <= 0
        or not isinstance(value["state"], str)
        or value["state"] not in {"RUNNING", "STOPPED", "FAILED"}
        or not _is_optional_protocol_text(value["failure_code"])
        or not _is_optional_protocol_text(value["failure_message"])
        or not _is_optional_protocol_text(value["connection_ref"])
    ):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding operation RuntimeStatus does not match its scope"
        )
    return {field: value[field] for field in _STATUS_FIELDS}


def _validate_observed_runtime_status(value: Any, binding_id: str) -> dict[str, Any]:
    """Validates a post-mutation binding status whose install may be rolled back."""
    if not isinstance(value, dict) or not set(_STATUS_FIELDS).issubset(value):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding readback returned an invalid RuntimeStatus"
        )
    if (
        value["binding_id"] != binding_id
        or not _is_protocol_text(value["binding_id"])
        or not _is_protocol_text(value["installation_id"])
        or type(value["generation"]) is not int
        or value["generation"] <= 0
        or not isinstance(value["state"], str)
        or value["state"] not in {"RUNNING", "STOPPED", "FAILED"}
        or not _is_optional_protocol_text(value["failure_code"])
        or not _is_optional_protocol_text(value["failure_message"])
        or not _is_optional_protocol_text(value["connection_ref"])
    ):
        raise PackageRuntimeError(
            "PACKAGE_RUNTIME_PROTOCOL_INVALID", "binding readback RuntimeStatus is invalid"
        )
    return {field: value[field] for field in _STATUS_FIELDS}


def _is_protocol_text(value: Any, *, max_length: int = 4096) -> bool:
    """Returns whether a wire identifier is bounded, non-empty text without controls."""
    return (
        isinstance(value, str)
        and bool(value.strip())
        and len(value) <= max_length
        and not any(ord(character) < 32 for character in value)
    )


def _is_optional_protocol_text(value: Any) -> bool:
    """Validates optional status strings without requiring failure metadata."""
    return value is None or _is_protocol_text(value)


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Rejects duplicate JSON keys so response validation has one meaning."""
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("JSON object contains a duplicate key")
        result[key] = value
    return result


def _secret_values(source_token: str | None, fields: Mapping[str, Any]) -> tuple[str, ...]:
    """Collects known secrets so a daemon error cannot echo them to callers."""
    values: set[str] = set()
    if source_token:
        values.add(source_token)
    environment = fields.get("environment")
    if isinstance(environment, Mapping):
        values.update(value for value in environment.values() if isinstance(value, str) and value)
    return tuple(sorted(values, key=len, reverse=True))


def _redact(message: str, secrets: tuple[str, ...]) -> str:
    """Redacts supplied secrets and sensitive key/value pairs from error text."""
    safe = message
    for secret in secrets:
        safe = safe.replace(secret, "[REDACTED]")
    safe = _SENSITIVE_ASSIGNMENT.sub(r"\1[REDACTED]\3", safe)
    safe = _SENSITIVE_INLINE.sub(r"\1[REDACTED]", safe)
    safe = "".join(character for character in safe if character >= " " or character in "\t")
    if len(safe) > _MAX_ERROR_TEXT_CHARS:
        safe = safe[:_MAX_ERROR_TEXT_CHARS] + "…"
    return safe
