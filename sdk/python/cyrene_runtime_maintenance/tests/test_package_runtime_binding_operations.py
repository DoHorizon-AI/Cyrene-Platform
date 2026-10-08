"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 test_package_runtime_binding_operations.py                      │
│  Module: cyrene_runtime_maintenance.tests.binding_operations         │
│  Role: Source owner completion tests for admitted binding changes.   │
│                                                                      │
│  模块职责：验证持久提交回调与维护门禁完成之间的安全顺序。               │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import socket
import threading
import time
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path
from typing import Any

import pytest

from cyrene_runtime_maintenance import (
    BindingOperationReceipt,
    BindingOperationScope,
    MaintenanceError,
    PackageRuntimeClient,
    PackageRuntimeError,
    RuntimeMaintenanceClient,
)
from cyrene_runtime_maintenance.package_runtime import _MAX_CONTROL_LINE_BYTES

_SOURCE_ID = "yield-training"
_SOURCE_TOKEN = "product-source-secret"
_OPERATION_TOKEN = "opaque-broker-lease-secret"
_BINDING_PROTOCOL = "cyrene.runtime-maintenance.binding-operations.v1"
_AUTHORITY = {
    "authority": "platform_package_runtime",
    "protocol_version": "cy-package-runtime.control.v1",
    "catalog_generation": 7,
    "capabilities": ["cy-package-runtime.binding-operation-admission.v1"],
}
_STATUS = {
    "binding_id": "binding-a",
    "installation_id": "install-a",
    "generation": 4,
    "state": "RUNNING",
    "failure_code": None,
    "failure_message": None,
    "connection_ref": "unix:///private/runtime/connection",
}
_INSTALLATION = {
    "installation_id": "install-a",
    "package_id": "package-a",
    "state": "INSTALLED",
}


def _start_server(
    socket_path: Path,
    responders: Iterable[Callable[[dict[str, Any]], bytes | dict[str, Any] | None]],
) -> tuple[threading.Thread, list[dict[str, Any]], list[BaseException]]:
    """Serves a finite request script over a deterministic local UDS."""
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    listener.listen(8)
    requests: list[dict[str, Any]] = []
    errors: list[BaseException] = []

    def serve() -> None:
        workers: list[threading.Thread] = []

        def handle(
            connection: socket.socket, respond: Callable[[dict[str, Any]], bytes | dict[str, Any] | None]
        ) -> None:
            try:
                with connection:
                    stream = connection.makefile("rb")
                    line = stream.readline(_MAX_CONTROL_LINE_BYTES + 1)
                    request = json.loads(line)
                    requests.append(request)
                    response = respond(request)
                    if isinstance(response, dict):
                        payload = json.dumps(response, separators=(",", ":")).encode() + b"\n"
                    else:
                        payload = response
                    if payload is not None:
                        try:
                            connection.sendall(payload)
                        except OSError:
                            pass
            except BaseException as error:  # Propagate worker failures to the test thread.
                errors.append(error)

        with listener:
            for respond in responders:
                try:
                    connection, _address = listener.accept()
                except BaseException as error:  # Propagate worker failures to the test thread.
                    errors.append(error)
                    return
                worker = threading.Thread(target=handle, args=(connection, respond), daemon=True)
                worker.start()
                workers.append(worker)
        for worker in workers:
            worker.join(timeout=2)

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return thread, requests, errors


def _join_server(
    server: tuple[threading.Thread, list[dict[str, Any]], list[BaseException]],
) -> list[dict[str, Any]]:
    thread, requests, errors = server
    thread.join(timeout=2)
    assert not thread.is_alive(), "fake UDS server did not finish its request script"
    assert errors == []
    return requests


def _ok(request: dict[str, Any], result: dict[str, Any]) -> dict[str, Any]:
    return {"request_id": request["request_id"], "ok": True, "result": result}


def _receipt(
    request: dict[str, Any],
    *,
    operation: str = "activate",
    overrides: dict[str, Any] | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "request_id": request["request_id"],
        "source_id": _SOURCE_ID,
        "protocol_version": _BINDING_PROTOCOL,
        "scope": {
            "binding_id": "binding-a",
            "package_id": "package-a",
            "installation_id": "install-a",
            "operation": operation,
        },
        "catalog_generation": 7,
        "gate_generation": 12,
        "operation_token": _OPERATION_TOKEN,
        "already_in_flight": False,
        "already_completed": False,
    }
    if overrides:
        value.update(overrides)
    return value


def _client(
    package_socket: Path,
    broker_socket: Path,
    *,
    timeout_seconds: float = 5.0,
) -> PackageRuntimeClient:
    broker = RuntimeMaintenanceClient(
        broker_socket,
        source_id=_SOURCE_ID,
        source_token=_SOURCE_TOKEN,
        catalog_generation=7,
        timeout_seconds=timeout_seconds,
    )
    return PackageRuntimeClient(
        package_socket,
        source_id=_SOURCE_ID,
        source_token=_SOURCE_TOKEN,
        timeout_seconds=timeout_seconds,
        maintenance_client=broker,
    )


def _persist_intent_noop(_request_id: str, _scope: BindingOperationScope) -> None:
    """Accepts the test's generated stable request ID without external storage."""
    return None


def _package_responders(
    operation_result: Callable[[dict[str, Any]], dict[str, Any]],
    *,
    authority: dict[str, Any] | None = None,
) -> list[Callable[[dict[str, Any]], dict[str, Any]]]:
    return [
        lambda request: _ok(request, authority or _AUTHORITY),
        lambda request: _ok(request, operation_result(request)),
    ]


def test_persist_callback_runs_before_exact_broker_completion(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    events: list[str] = []

    def operation_result(request: dict[str, Any]) -> dict[str, Any]:
        return {**_STATUS, "binding_operation": _receipt(request)}

    package_server = _start_server(package_socket, _package_responders(operation_result))

    def complete(request: dict[str, Any]) -> dict[str, Any]:
        events.append("complete")
        return {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 13},
        }

    broker_server = _start_server(broker_socket, [complete])
    client = _client(package_socket, broker_socket)
    persisted: list[tuple[dict[str, Any], BindingOperationReceipt]] = []
    intent: list[tuple[str, BindingOperationScope]] = []

    def persist_intent(request_id: str, scope: BindingOperationScope) -> None:
        intent.append((request_id, scope))
        events.append("intent")

    def persist(status: Any, receipt: BindingOperationReceipt) -> None:
        persisted.append((dict(status), receipt))
        events.append("persist")

    result = client.activate_and_persist(
        "binding-a",
        "install-a",
        package_id="package-a",
        persist_intent=persist_intent,
        persist=persist,
        environment={"MODE": "training"},
    )

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert events == ["intent", "persist", "complete"]
    assert result == _STATUS
    assert persisted[0][0] == _STATUS
    receipt = persisted[0][1]
    assert intent == [(receipt.request_id, receipt.scope)]
    assert receipt.request_id == package_requests[1]["request_id"]
    assert "operation_token" not in repr(receipt)
    assert package_requests[1] == {
        "request_id": package_requests[1]["request_id"],
        "operation": "activate",
        "binding_id": "binding-a",
        "installation_id": "install-a",
        "environment": {"MODE": "training"},
        "auth": {"source_id": _SOURCE_ID, "source_token": _SOURCE_TOKEN},
        "catalog_generation": 7,
    }
    assert broker_requests == [
        {
            "request_id": receipt.request_id,
            "method": "CompleteBindingOperation",
            "auth": {"source_id": _SOURCE_ID, "source_token": _SOURCE_TOKEN},
            "params": {
                "source_id": _SOURCE_ID,
                "expected_catalog_generation": 7,
                "binding_id": "binding-a",
                "package_id": "package-a",
                "installation_id": "install-a",
                "operation": "activate",
                "operation_token": _OPERATION_TOKEN,
            },
            "protocol_version": _BINDING_PROTOCOL,
        }
    ]


def test_cataloged_standalone_source_uses_uds_and_broker_admission(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    token_path = tmp_path / "standalone-source.token"
    source_id = "cyrene-standalone-plugins"
    source_token = "standalone-owner-secret"
    binding_id = "plugin.catalog.binding"
    token_path.write_text(source_token, encoding="utf-8")
    token_path.chmod(0o600)

    def activate(request: dict[str, Any]) -> dict[str, Any]:
        assert request["auth"] == {"source_id": source_id, "source_token": source_token}
        scope = {
            "binding_id": binding_id,
            "package_id": "package-a",
            "installation_id": "install-a",
            "operation": "activate",
        }
        receipt = _receipt(request, overrides={"source_id": source_id, "scope": scope})
        status = {**_STATUS, "binding_id": binding_id}
        return _ok(request, {**status, "binding_operation": receipt})

    package_server = _start_server(
        package_socket,
        [lambda request: _ok(request, _AUTHORITY), activate],
    )
    broker_server = _start_server(
        broker_socket,
        [lambda request: {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 13},
        }],
    )
    client = PackageRuntimeClient.from_source_secret(
        source_id,
        token_path,
        socket_path=package_socket,
        catalog_generation=7,
        maintenance_socket_path=broker_socket,
    )

    status = client.activate_and_persist(
        binding_id,
        "install-a",
        package_id="package-a",
        persist_intent=_persist_intent_noop,
        persist=lambda _status, _receipt_value: None,
    )

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert status["binding_id"] == binding_id
    assert package_requests[1]["auth"] == {"source_id": source_id, "source_token": source_token}
    assert package_requests[1]["binding_id"] == binding_id
    assert broker_requests[0]["auth"] == {"source_id": source_id, "source_token": source_token}
    assert broker_requests[0]["params"]["source_id"] == source_id
    assert broker_requests[0]["params"]["binding_id"] == binding_id
    assert broker_requests[0]["params"]["operation"] == "activate"


def test_persistence_failure_leaves_receipt_pending_without_broker_call(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker-does-not-exist.sock"

    package_server = _start_server(
        package_socket,
        _package_responders(lambda request: {**_STATUS, "binding_operation": _receipt(request)}),
    )
    client = _client(package_socket, broker_socket)

    class PersistFailure(RuntimeError):
        pass

    def fail_persist(_status: Any, _receipt_value: BindingOperationReceipt) -> None:
        raise PersistFailure("owner record commit failed")

    with pytest.raises(PersistFailure, match="owner record commit failed"):
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=fail_persist,
        )

    requests = _join_server(package_server)
    assert [request["operation"] for request in requests] == ["authority", "activate"]


@pytest.mark.parametrize("callback_mode", ["async", "non_none"])
def test_non_synchronous_or_non_none_persistence_callback_cannot_complete_gate(
    tmp_path: Path, callback_mode: str
) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker-does-not-exist.sock"
    package_server = _start_server(
        package_socket,
        _package_responders(lambda request: {**_STATUS, "binding_operation": _receipt(request)}),
    )
    client = _client(package_socket, broker_socket)

    if callback_mode == "async":

        async def persist(_status: Any, _receipt_value: BindingOperationReceipt) -> None:
            return None

        expected_error = "finish synchronously"
    else:

        def persist(_status: Any, _receipt_value: BindingOperationReceipt) -> bool:
            return False

        expected_error = "return None"

    with pytest.raises(TypeError, match=expected_error):
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=persist,
        )

    requests = _join_server(package_server)
    assert [request["operation"] for request in requests] == ["authority", "activate"]


@pytest.mark.parametrize(
    ("method_name", "operation", "status_state"),
    [
        ("recover_binding", "recover", "RUNNING"),
        ("deactivate", "deactivate", "STOPPED"),
    ],
)
def test_recover_and_deactivate_use_the_expected_receipt_scope(
    tmp_path: Path, method_name: str, operation: str, status_state: str
) -> None:
    package_socket = tmp_path / f"{operation}-package.sock"
    broker_socket = tmp_path / f"{operation}-broker.sock"
    status = {
        **_STATUS,
        "state": status_state,
        "connection_ref": None if operation == "deactivate" else _STATUS["connection_ref"],
    }
    package_server = _start_server(
        package_socket,
        _package_responders(lambda request: {**status, "binding_operation": _receipt(request, operation=operation)}),
    )
    broker_server = _start_server(
        broker_socket,
        [lambda request: {"request_id": request["request_id"], "result": {"completed": True, "gate_generation": 13}}],
    )
    client = _client(package_socket, broker_socket)

    if method_name == "recover_binding":
        method = client.recover_and_persist
        args = ("binding-a",)
        kwargs = {"package_id": "package-a", "installation_id": "install-a"}
    else:
        method = client.deactivate_and_persist
        args = ("binding-a",)
        kwargs = {"package_id": "package-a", "installation_id": "install-a"}

    result = method(
        *args,
        **kwargs,
        persist_intent=_persist_intent_noop,
        persist=lambda _status, _receipt_value: None,
    )

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert result["state"] == status_state
    assert package_requests[1]["operation"] == ("recover_binding" if operation == "recover" else "deactivate")
    assert broker_requests[0]["params"]["operation"] == operation


@pytest.mark.parametrize(
    "change",
    [
        "missing_field",
        "wrong_protocol",
        "wrong_request_id",
        "wrong_source",
        "wrong_catalog_generation",
        "wrong_scope",
        "invalid_flags",
        "empty_token",
    ],
)
def test_invalid_receipt_fails_before_persist_or_complete(tmp_path: Path, change: str) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker-does-not-exist.sock"

    def operation_result(request: dict[str, Any]) -> dict[str, Any]:
        receipt = _receipt(request)
        if change == "missing_field":
            del receipt["gate_generation"]
        elif change == "wrong_protocol":
            receipt["protocol_version"] = "unsupported"
        elif change == "wrong_request_id":
            receipt["request_id"] = "other-request"
        elif change == "wrong_source":
            receipt["source_id"] = "other-source"
        elif change == "wrong_catalog_generation":
            receipt["catalog_generation"] = 8
        elif change == "wrong_scope":
            receipt["scope"]["package_id"] = "other-package"
        elif change == "invalid_flags":
            receipt["already_in_flight"] = True
            receipt["already_completed"] = True
        elif change == "empty_token":
            receipt["operation_token"] = ""
        return {**_STATUS, "binding_operation": receipt}

    package_server = _start_server(package_socket, _package_responders(operation_result))
    client = _client(package_socket, broker_socket)
    persisted: list[bool] = []

    with pytest.raises(PackageRuntimeError) as error:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, _receipt_value: persisted.append(True),
        )

    requests = _join_server(package_server)
    assert error.value.code == "PACKAGE_RUNTIME_PROTOCOL_INVALID"
    assert persisted == []
    assert [request["operation"] for request in requests] == ["authority", "activate"]


def test_public_mutation_uses_caller_request_id_and_retains_it_on_protocol_error(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"

    def responder(request: dict[str, Any]) -> dict[str, Any]:
        if request["operation"] == "authority":
            return _ok(request, _AUTHORITY)
        return {"request_id": "unexpected-id", "ok": True, "result": {}}

    package_server = _start_server(package_socket, [responder, responder])
    client = PackageRuntimeClient(package_socket, source_id=_SOURCE_ID, source_token=_SOURCE_TOKEN)

    with pytest.raises(PackageRuntimeError) as error:
        client.activate(
            "binding-a",
            "install-a",
            package_id="package-a",
            request_id="owner-request-1",
        )

    requests = _join_server(package_server)
    assert error.value.code == "PACKAGE_RUNTIME_PROTOCOL_INVALID"
    assert error.value.request_id == "owner-request-1"
    assert requests[1]["request_id"] == "owner-request-1"
    assert len([request for request in requests if request["operation"] == "activate"]) == 1


def test_pending_error_exposes_typed_receipt_without_token_in_error_text(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker-does-not-exist.sock"

    def pending(request: dict[str, Any]) -> dict[str, Any]:
        receipt = _receipt(request, overrides={"already_in_flight": True})
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "BINDING_OPERATION_PENDING",
                "message": f"retry after owner reconciliation token={_OPERATION_TOKEN}",
                "remediation": "persist the owner record and reconcile",
                "binding_operation": receipt,
            },
        }

    package_server = _start_server(
        package_socket,
        [lambda request: _ok(request, _AUTHORITY), pending],
    )
    client = _client(package_socket, broker_socket)
    persisted: list[bool] = []

    with pytest.raises(PackageRuntimeError) as error:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, _receipt_value: persisted.append(True),
        )

    _join_server(package_server)
    assert error.value.code == "BINDING_OPERATION_PENDING"
    assert isinstance(error.value.pending_binding_operation, BindingOperationReceipt)
    assert error.value.pending_binding_operation.already_in_flight
    assert _OPERATION_TOKEN not in str(error.value)
    assert _OPERATION_TOKEN not in repr(error.value)
    assert _OPERATION_TOKEN not in repr(error.value.pending_binding_operation)
    assert persisted == []


def test_complete_failure_keeps_owner_receipt_and_redacts_token(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    persisted: list[BindingOperationReceipt] = []
    package_server = _start_server(
        package_socket,
        _package_responders(lambda request: {**_STATUS, "binding_operation": _receipt(request)}),
    )
    broker_server = _start_server(
        broker_socket,
        [
            lambda request: {
                "request_id": request["request_id"],
                "error": {
                    "code": "MAINTENANCE_ADMISSION_DENIED",
                    "message": f"completion denied token={_OPERATION_TOKEN}",
                },
            }
        ],
    )
    client = _client(package_socket, broker_socket)

    with pytest.raises(MaintenanceError) as error:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, receipt: persisted.append(receipt),
        )

    _join_server(package_server)
    _join_server(broker_server)
    assert error.value.code == "MAINTENANCE_ADMISSION_DENIED"
    assert _OPERATION_TOKEN not in str(error.value)
    assert len(persisted) == 1
    assert persisted[0].operation_token == _OPERATION_TOKEN


def test_explicit_failed_activation_outcome_is_persisted_before_lease_completion(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    events: list[str] = []
    persisted: list[BindingOperationReceipt] = []

    def authority(request: dict[str, Any]) -> dict[str, Any]:
        events.append("authority")
        return _ok(request, _AUTHORITY)

    package_server = _start_server(package_socket, [authority])

    def complete(request: dict[str, Any]) -> dict[str, Any]:
        assert events == ["authority", "persist_outcome"]
        events.append("complete")
        assert request["params"]["operation_token"] == _OPERATION_TOKEN
        return {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 12},
        }

    broker_server = _start_server(broker_socket, [complete])
    client = _client(package_socket, broker_socket)
    receipt = BindingOperationReceipt(
        request_id="failed-first-activation",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=12,
        already_in_flight=False,
        already_completed=False,
    )

    def persist_outcome(pending_receipt: BindingOperationReceipt) -> None:
        assert pending_receipt is receipt
        persisted.append(pending_receipt)
        events.append("persist_outcome")

    client.commit_binding_operation_outcome(receipt, persist_outcome=persist_outcome)

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert events == ["authority", "persist_outcome", "complete"]
    assert persisted == [receipt]
    assert [request["operation"] for request in package_requests] == ["authority"]
    assert broker_requests[0]["method"] == "CompleteBindingOperation"
    assert broker_requests[0]["request_id"] == receipt.request_id


@pytest.mark.parametrize("reply_shape", ["wrong_request_id", "invalid_generation", "extra_field"])
def test_malformed_complete_reply_keeps_persisted_receipt_pending(tmp_path: Path, reply_shape: str) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    package_server = _start_server(
        package_socket,
        _package_responders(lambda request: {**_STATUS, "binding_operation": _receipt(request)}),
    )

    def broker_reply(request: dict[str, Any]) -> dict[str, Any]:
        request_id = "wrong-request" if reply_shape == "wrong_request_id" else request["request_id"]
        result: dict[str, Any] = {"completed": True, "gate_generation": 13}
        if reply_shape == "invalid_generation":
            result["gate_generation"] = True
        elif reply_shape == "extra_field":
            result["unexpected"] = "value"
        return {"request_id": request_id, "result": result}

    broker_server = _start_server(broker_socket, [broker_reply])
    client = _client(package_socket, broker_socket)
    persisted: list[BindingOperationReceipt] = []

    with pytest.raises(MaintenanceError) as error:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, receipt: persisted.append(receipt),
        )

    package_requests = _join_server(package_server)
    _join_server(broker_server)
    assert error.value.code == "MAINTENANCE_PROTOCOL_INVALID"
    assert len(persisted) == 1
    assert persisted[0].request_id == package_requests[1]["request_id"]


def test_missing_capability_blocks_mutation_but_keeps_read_methods_available(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    no_capability = {**_AUTHORITY, "capabilities": []}
    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, no_capability),
            lambda request: _ok(request, {"binding_id": "binding-a", "state": "STOPPED"}),
        ],
    )
    client = PackageRuntimeClient(package_socket, source_id=_SOURCE_ID, source_token=_SOURCE_TOKEN)

    with pytest.raises(PackageRuntimeError) as error:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, _receipt_value: None,
        )
    assert error.value.code == "PACKAGE_RUNTIME_CAPABILITY_REQUIRED"
    assert client.runtime_status("binding-a")["state"] == "STOPPED"

    requests = _join_server(package_server)
    assert [request["operation"] for request in requests] == ["authority", "runtime_status"]


def test_complete_rejects_root_operator_and_generation_mismatch() -> None:
    receipt = BindingOperationReceipt(
        request_id="request-1",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=12,
        already_in_flight=False,
        already_completed=False,
    )
    operator_client = RuntimeMaintenanceClient(operator_token="root-token")
    with pytest.raises(MaintenanceError) as operator_error:
        operator_client._complete_binding_operation_after_owner_commit(receipt, allow_in_flight=False)
    assert operator_error.value.code == "ACTIVITY_SOURCE_AUTH_REQUIRED"

    wrong_generation_client = RuntimeMaintenanceClient(
        source_id=_SOURCE_ID,
        source_token=_SOURCE_TOKEN,
        catalog_generation=8,
    )
    with pytest.raises(MaintenanceError) as generation_error:
        wrong_generation_client._complete_binding_operation_after_owner_commit(receipt, allow_in_flight=False)
    assert generation_error.value.code == "CATALOG_GENERATION_CHANGED"


def test_lost_admission_reply_replays_same_request_id_then_reconciles_pending_lease(
    tmp_path: Path,
) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    durable_intents: dict[str, BindingOperationScope] = {}
    owner_commits: list[tuple[dict[str, Any], dict[str, Any], BindingOperationReceipt]] = []

    def authority(request: dict[str, Any]) -> dict[str, Any]:
        assert durable_intents, "the owner intent must be durable before the first socket request"
        return _ok(request, _AUTHORITY)

    def lose_first_mutation_reply(request: dict[str, Any]) -> dict[str, Any]:
        assert request["request_id"] in durable_intents
        time.sleep(0.15)
        return _ok(request, {**_STATUS, "binding_operation": _receipt(request)})

    def pending_replay(request: dict[str, Any]) -> dict[str, Any]:
        assert request["request_id"] in durable_intents
        receipt = _receipt(request, overrides={"already_in_flight": True})
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "BINDING_OPERATION_PENDING",
                "message": "matching operation remains pending",
                "remediation": "reconcile owner state",
                "binding_operation": receipt,
            },
        }

    package_server = _start_server(
        package_socket,
        [
            authority,
            lose_first_mutation_reply,
            authority,
            pending_replay,
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, _STATUS),
            lambda request: _ok(request, _INSTALLATION),
        ],
    )

    def complete(request: dict[str, Any]) -> dict[str, Any]:
        return {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 13},
        }

    broker_server = _start_server(broker_socket, [complete])
    client = _client(package_socket, broker_socket, timeout_seconds=0.04)

    def persist_intent(request_id: str, scope: BindingOperationScope) -> None:
        existing = durable_intents.setdefault(request_id, scope)
        if existing != scope:
            raise AssertionError("a stable request ID cannot change its operation scope")

    with pytest.raises(PackageRuntimeError) as lost_reply:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=persist_intent,
            persist=lambda _status, _receipt_value: None,
        )
    assert lost_reply.value.code == "CONTROL_TIMEOUT"
    assert lost_reply.value.request_id in durable_intents
    stable_request_id = lost_reply.value.request_id

    # A new SDK instance represents the Product owner recovering from its
    # durable intent after the original process lost the mutation response.
    recovered_client = _client(package_socket, broker_socket, timeout_seconds=0.04)
    with pytest.raises(PackageRuntimeError) as replay:
        recovered_client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=persist_intent,
            persist=lambda _status, _receipt_value: None,
            request_id=stable_request_id,
        )
    assert replay.value.code == "BINDING_OPERATION_PENDING"
    receipt = replay.value.pending_binding_operation
    assert receipt is not None and receipt.already_in_flight

    def reconcile(
        status: dict[str, Any],
        installation: dict[str, Any],
        pending_receipt: BindingOperationReceipt,
    ) -> None:
        assert durable_intents[pending_receipt.request_id] == pending_receipt.scope
        owner_commits.append((status, installation, pending_receipt))

    result = client.reconcile_binding_operation_and_persist(receipt, reconcile=reconcile)

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    mutation_requests = [request for request in package_requests if request["operation"] == "activate"]
    assert [request["request_id"] for request in mutation_requests] == [stable_request_id, stable_request_id]
    assert len(owner_commits) == 1
    assert owner_commits[0][0] == _STATUS
    assert owner_commits[0][1] == _INSTALLATION
    assert owner_commits[0][2] is receipt
    assert result.runtime_status == _STATUS
    assert result.installation == _INSTALLATION
    assert result.binding_operation is receipt
    assert [request["operation"] for request in package_requests] == [
        "authority",
        "activate",
        "authority",
        "activate",
        "authority",
        "runtime_status",
        "get_installation",
    ]
    assert broker_requests[0]["request_id"] == stable_request_id
    assert broker_requests[0]["params"]["operation_token"] == _OPERATION_TOKEN


def test_pending_first_activation_failure_reconciles_absent_binding_record(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    request_id = "stable-first-activation-failure"
    observed_outcomes: list[tuple[Mapping[str, Any] | None, BindingOperationReceipt]] = []

    def lost_first_activation_reply(_request: dict[str, Any]) -> None:
        # The daemon completed the failed activation before this dropped reply;
        # the owner must learn the outcome from serialized status readback.
        return None

    def pending_replay(request: dict[str, Any]) -> dict[str, Any]:
        receipt = _receipt(request, overrides={"already_in_flight": True})
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "BINDING_OPERATION_PENDING",
                "message": "matching operation remains pending",
                "remediation": "reconcile owner state",
                "binding_operation": receipt,
            },
        }

    def missing_binding(request: dict[str, Any]) -> dict[str, Any]:
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "BINDING_NOT_FOUND",
                "message": "first activation failed before a binding record was created",
                "remediation": "record the failed activation outcome",
            },
        }

    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, _AUTHORITY),
            lost_first_activation_reply,
            lambda request: _ok(request, _AUTHORITY),
            pending_replay,
            lambda request: _ok(request, _AUTHORITY),
            missing_binding,
            lambda request: _ok(request, _INSTALLATION),
        ],
    )

    def complete(request: dict[str, Any]) -> dict[str, Any]:
        assert observed_outcomes
        return {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 12},
        }

    broker_server = _start_server(broker_socket, [complete])
    client = _client(package_socket, broker_socket)
    with pytest.raises(PackageRuntimeError) as lost:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, _receipt_value: None,
            request_id=request_id,
        )
    assert lost.value.pending_binding_operation is None

    recovered_client = _client(package_socket, broker_socket)
    with pytest.raises(PackageRuntimeError) as replay:
        recovered_client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=_persist_intent_noop,
            persist=lambda _status, _receipt_value: None,
            request_id=request_id,
        )
    receipt = replay.value.pending_binding_operation
    assert replay.value.code == "BINDING_OPERATION_PENDING"
    assert receipt is not None and receipt.already_in_flight

    result = recovered_client.reconcile_binding_operation_and_persist(
        receipt,
        reconcile=lambda status, _installation, saved_receipt: observed_outcomes.append(
            (status, saved_receipt)
        ),
    )

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert observed_outcomes == [(None, receipt)]
    assert result.runtime_status is None
    assert result.installation == _INSTALLATION
    assert result.binding_operation is receipt
    assert [request["operation"] for request in package_requests] == [
        "authority",
        "activate",
        "authority",
        "activate",
        "authority",
        "runtime_status",
        "get_installation",
    ]
    assert broker_requests[0]["request_id"] == request_id


def test_lost_complete_reply_reconciles_persisted_fresh_receipt_without_changing_flags(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker.sock"
    request_id = "stable-completion-request"
    durable_intents: dict[str, BindingOperationScope] = {}
    durable_owner_records: dict[str, tuple[Mapping[str, Any], BindingOperationReceipt]] = {}
    owner_reconciliations: list[tuple[Mapping[str, Any], Mapping[str, Any], BindingOperationReceipt]] = []

    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, {**_STATUS, "binding_operation": _receipt(request)}),
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, _STATUS),
            lambda request: _ok(request, _INSTALLATION),
        ],
    )
    committed_completions: list[dict[str, Any]] = []

    def lose_committed_completion_reply(request: dict[str, Any]) -> None:
        # The Broker commits before the connection drops; the caller must keep
        # its exact persisted receipt and retry only Complete during recovery.
        committed_completions.append(request)
        return None

    def idempotent_completion(request: dict[str, Any]) -> dict[str, Any]:
        assert committed_completions == [request]
        return {
            "request_id": request["request_id"],
            "result": {"completed": True, "gate_generation": 13},
        }

    broker_server = _start_server(broker_socket, [lose_committed_completion_reply, idempotent_completion])
    client = _client(package_socket, broker_socket)
    scope = BindingOperationScope("binding-a", "package-a", "install-a", "activate")

    def persist_intent(stable_id: str, intended_scope: BindingOperationScope) -> None:
        assert stable_id == request_id
        assert intended_scope == scope
        durable_intents[stable_id] = intended_scope

    def persist_initial(status: Mapping[str, Any], receipt: BindingOperationReceipt) -> None:
        assert durable_intents[receipt.request_id] == receipt.scope
        assert receipt.request_id == request_id
        assert not receipt.already_in_flight and not receipt.already_completed
        durable_owner_records[receipt.request_id] = (dict(status), receipt)

    with pytest.raises(MaintenanceError) as lost_reply:
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=persist_intent,
            persist=persist_initial,
            request_id=request_id,
        )

    assert lost_reply.value.request_id == request_id
    saved_status, saved_receipt = durable_owner_records[request_id]
    assert saved_status == _STATUS
    assert saved_receipt.already_in_flight is False
    assert saved_receipt.already_completed is False

    def reconcile(
        status: Mapping[str, Any],
        installation: Mapping[str, Any],
        receipt: BindingOperationReceipt,
    ) -> None:
        assert durable_intents[receipt.request_id] == receipt.scope
        assert durable_owner_records[receipt.request_id][1] is receipt
        assert status == _STATUS
        assert installation == _INSTALLATION
        assert receipt is saved_receipt
        assert receipt.already_in_flight is False
        assert receipt.already_completed is False
        owner_reconciliations.append((status, installation, receipt))

    # Startup recovery uses a fresh client and the exact durably stored receipt.
    recovered_client = _client(package_socket, broker_socket)
    result = recovered_client.reconcile_binding_operation_and_persist(saved_receipt, reconcile=reconcile)

    package_requests = _join_server(package_server)
    broker_requests = _join_server(broker_server)
    assert [request["operation"] for request in package_requests] == [
        "authority",
        "activate",
        "authority",
        "runtime_status",
        "get_installation",
    ]
    assert len(owner_reconciliations) == 1
    assert owner_reconciliations[0][2] is saved_receipt
    assert result.binding_operation is saved_receipt
    assert saved_receipt.already_in_flight is False
    assert saved_receipt.already_completed is False
    assert len(broker_requests) == 2
    assert broker_requests[0] == broker_requests[1]
    assert broker_requests[0]["request_id"] == request_id
    assert broker_requests[0]["params"]["operation_token"] == _OPERATION_TOKEN


def test_failed_pending_reconciliation_does_not_complete_or_reissue_mutation(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    broker_socket = tmp_path / "broker-must-not-be-called.sock"
    receipt = BindingOperationReceipt(
        request_id="stable-request-1",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=12,
        already_in_flight=True,
        already_completed=False,
    )
    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, _STATUS),
            lambda request: _ok(request, _INSTALLATION),
        ],
    )
    client = _client(package_socket, broker_socket)

    def fail_commit(
        _status: Mapping[str, Any],
        _installation: Mapping[str, Any],
        _receipt_value: BindingOperationReceipt,
    ) -> None:
        raise RuntimeError(f"owner database unavailable; token={_OPERATION_TOKEN}")

    with pytest.raises(PackageRuntimeError) as error:
        client.reconcile_binding_operation_and_persist(receipt, reconcile=fail_commit)

    requests = _join_server(package_server)
    assert error.value.code == "OWNER_RECONCILIATION_FAILED"
    assert error.value.request_id == receipt.request_id
    assert _OPERATION_TOKEN not in str(error.value)
    assert [request["operation"] for request in requests] == [
        "authority",
        "runtime_status",
        "get_installation",
    ]


def test_pending_reconciliation_rejects_wrong_installation_before_owner_callback(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    receipt = BindingOperationReceipt(
        request_id="stable-request-2",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=13,
        already_in_flight=True,
        already_completed=False,
    )
    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, _STATUS),
            lambda request: _ok(
                request,
                {"installation_id": "install-a", "package_id": "other-package", "state": "INSTALLED"},
            ),
        ],
    )
    client = _client(package_socket, tmp_path / "broker-must-not-be-called.sock")
    callback_calls: list[bool] = []

    with pytest.raises(PackageRuntimeError) as error:
        client.reconcile_binding_operation_and_persist(
            receipt,
            reconcile=lambda _status, _installation, _receipt_value: callback_calls.append(True),
        )

    requests = _join_server(package_server)
    assert error.value.code == "PACKAGE_RUNTIME_SCOPE_MISMATCH"
    assert error.value.request_id == receipt.request_id
    assert callback_calls == []
    assert requests[-1]["operation"] == "get_installation"


@pytest.mark.parametrize("callback_mode", ["async", "non_none"])
def test_pending_reconciliation_requires_a_synchronous_durable_owner_commit(
    tmp_path: Path,
    callback_mode: str,
) -> None:
    package_socket = tmp_path / "package.sock"
    receipt = BindingOperationReceipt(
        request_id="stable-request-3",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=14,
        already_in_flight=True,
        already_completed=False,
    )
    package_server = _start_server(
        package_socket,
        [
            lambda request: _ok(request, _AUTHORITY),
            lambda request: _ok(request, _STATUS),
            lambda request: _ok(request, _INSTALLATION),
        ],
    )
    client = _client(package_socket, tmp_path / "broker-must-not-be-called.sock")

    if callback_mode == "async":

        async def reconcile(
            _status: Mapping[str, Any],
            _installation: Mapping[str, Any],
            _receipt_value: BindingOperationReceipt,
        ) -> None:
            return None

    else:

        def reconcile(
            _status: Mapping[str, Any],
            _installation: Mapping[str, Any],
            _receipt_value: BindingOperationReceipt,
        ) -> bool:
            return False

    with pytest.raises(PackageRuntimeError) as error:
        client.reconcile_binding_operation_and_persist(receipt, reconcile=reconcile)

    requests = _join_server(package_server)
    assert error.value.code == "OWNER_RECONCILIATION_INCOMPLETE"
    assert error.value.request_id == receipt.request_id
    assert [request["operation"] for request in requests] == [
        "authority",
        "runtime_status",
        "get_installation",
    ]


def test_pending_reconciliation_fails_closed_on_fresh_authority_generation_drift(tmp_path: Path) -> None:
    package_socket = tmp_path / "package.sock"
    receipt = BindingOperationReceipt(
        request_id="stable-request-4",
        source_id=_SOURCE_ID,
        protocol_version=_BINDING_PROTOCOL,
        catalog_generation=7,
        scope=BindingOperationScope("binding-a", "package-a", "install-a", "activate"),
        operation_token=_OPERATION_TOKEN,
        gate_generation=15,
        already_in_flight=True,
        already_completed=False,
    )
    package_server = _start_server(
        package_socket,
        [lambda request: _ok(request, {**_AUTHORITY, "catalog_generation": 8})],
    )
    client = _client(package_socket, tmp_path / "broker-must-not-be-called.sock")

    with pytest.raises(PackageRuntimeError) as error:
        client.reconcile_binding_operation_and_persist(
            receipt,
            reconcile=lambda _status, _installation, _receipt_value: None,
        )

    requests = _join_server(package_server)
    assert error.value.code == "CATALOG_GENERATION_CHANGED"
    assert error.value.request_id == receipt.request_id
    assert [request["operation"] for request in requests] == ["authority"]


@pytest.mark.parametrize("invalid_intent", ["async", "non_none", "invalid_request_id"])
def test_invalid_durable_intent_stops_before_any_network_request(
    tmp_path: Path,
    invalid_intent: str,
) -> None:
    client = _client(tmp_path / "no-network.sock", tmp_path / "broker.sock")

    if invalid_intent == "async":

        async def persist_intent(_request_id: str, _scope: BindingOperationScope) -> None:
            return None

        request_id = None
        expected_error = TypeError
    elif invalid_intent == "non_none":

        def persist_intent(_request_id: str, _scope: BindingOperationScope) -> bool:
            return False

        request_id = None
        expected_error = TypeError
    else:

        def persist_intent(_request_id: str, _scope: BindingOperationScope) -> None:
            return None

        request_id = "invalid\nrequest-id"
        expected_error = ValueError

    with pytest.raises(expected_error):
        client.activate_and_persist(
            "binding-a",
            "install-a",
            package_id="package-a",
            persist_intent=persist_intent,
            persist=lambda _status, _receipt_value: None,
            request_id=request_id,
        )
