"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 test_package_runtime.py                                          │
│  Module: cyrene_runtime_maintenance.tests.test_package_runtime      │
│  Role: Package Runtime JSONL client protocol and safety tests.       │
│                                                                      │
│  模块职责：验证 Package Runtime 客户端协议与凭证安全边界。             │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import socket
import threading
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

import pytest

from cyrene_runtime_maintenance import PackageRuntimeClient, PackageRuntimeError
from cyrene_runtime_maintenance.package_runtime import _MAX_CONTROL_LINE_BYTES

_AUTHORITY = {
    "authority": "platform_package_runtime",
    "protocol_version": "cy-package-runtime.control.v1",
    "catalog_generation": 7,
    "capabilities": ["cy-package-runtime.binding-operation-admission.v1"],
}


def _start_server(
    socket_path: Path,
    responders: Iterable[Callable[[dict[str, Any]], bytes | dict[str, Any] | None]],
) -> tuple[threading.Thread, list[dict[str, Any]]]:
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    listener.listen(8)
    requests: list[dict[str, Any]] = []
    handlers = iter(responders)

    def serve() -> None:
        with listener:
            for respond in handlers:
                try:
                    connection, _ = listener.accept()
                except OSError:
                    return
                with connection:
                    stream = connection.makefile("rb")
                    request_line = stream.readline(_MAX_CONTROL_LINE_BYTES + 1)
                    request = json.loads(request_line)
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

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return thread, requests


def _success(request: dict[str, Any], result: dict[str, Any]) -> dict[str, Any]:
    return {"request_id": request["request_id"], "ok": True, "result": result}


def _source_client(socket_path: Path, **kwargs: Any) -> PackageRuntimeClient:
    return PackageRuntimeClient(
        socket_path,
        source_id="yield-training",
        source_token="source-secret",
        **kwargs,
    )


def test_handshake_then_flattened_get_installation_request(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"
    thread, requests = _start_server(
        socket_path,
        [
            lambda request: _success(request, _AUTHORITY),
            lambda request: _success(request, {"installation_id": "install-1"}),
        ],
    )
    client = _source_client(socket_path)

    result = client.get_installation("install-1")

    thread.join(timeout=2)
    assert result["installation_id"] == "install-1"
    assert client.catalog_generation == 7
    assert requests[0]["operation"] == "authority"
    assert "catalog_generation" not in requests[0]
    assert requests[0]["auth"] == {
        "source_id": "yield-training",
        "source_token": "source-secret",
    }
    assert requests[1] == {
        "request_id": requests[1]["request_id"],
        "operation": "get_installation",
        "installation_id": "install-1",
        "auth": {"source_id": "yield-training", "source_token": "source-secret"},
        "catalog_generation": 7,
    }


@pytest.mark.parametrize(
    ("authority_result", "error_code"),
    [
        ({**_AUTHORITY, "protocol_version": "unsupported"}, "PACKAGE_RUNTIME_PROTOCOL_MISMATCH"),
        ({**_AUTHORITY, "authority": "other-runtime"}, "PACKAGE_RUNTIME_AUTHORITY_MISMATCH"),
        ({**_AUTHORITY, "catalog_generation": True}, "PACKAGE_RUNTIME_PROTOCOL_INVALID"),
        (
            {key: value for key, value in _AUTHORITY.items() if key != "catalog_generation"},
            "PACKAGE_RUNTIME_PROTOCOL_INVALID",
        ),
    ],
)
def test_authority_rejects_wrong_protocol_identity_and_generation(
    tmp_path: Path, authority_result: dict[str, Any], error_code: str
) -> None:
    socket_path = tmp_path / "control.sock"
    thread, requests = _start_server(socket_path, [lambda request: _success(request, authority_result)])

    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path).activate("binding-a", "install-a", package_id="package-a")

    thread.join(timeout=2)
    assert error.value.code == error_code
    assert len(requests) == 1
    assert requests[0]["operation"] == "authority"


def test_authority_rejects_request_id_mismatch(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"

    def wrong_id(request: dict[str, Any]) -> dict[str, Any]:
        return {"request_id": "wrong-id", "ok": True, "result": _AUTHORITY}

    thread, requests = _start_server(socket_path, [wrong_id])
    with pytest.raises(PackageRuntimeError, match="mismatched request_id") as error:
        _source_client(socket_path).runtime_status("binding-a")

    thread.join(timeout=2)
    assert error.value.code == "PACKAGE_RUNTIME_PROTOCOL_INVALID"
    assert len(requests) == 1


def test_operation_rejects_response_request_id_mismatch(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"

    def wrong_id(request: dict[str, Any]) -> dict[str, Any]:
        if request["operation"] == "authority":
            return _success(request, _AUTHORITY)
        return {"request_id": "wrong-id", "ok": True, "result": {}}

    thread, requests = _start_server(socket_path, [wrong_id, wrong_id])
    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path).get_installation("install-a")

    thread.join(timeout=2)
    assert error.value.code == "PACKAGE_RUNTIME_PROTOCOL_INVALID"
    assert len(requests) == 2


def test_changed_catalog_generation_fails_closed_and_keeps_expected_value(
    tmp_path: Path,
) -> None:
    socket_path = tmp_path / "control.sock"
    thread, requests = _start_server(
        socket_path,
        [lambda request: _success(request, {**_AUTHORITY, "catalog_generation": 8})],
    )
    client = _source_client(socket_path, catalog_generation=7)

    with pytest.raises(PackageRuntimeError) as error:
        client.activate("binding-a", "install-a", package_id="package-a")

    thread.join(timeout=2)
    assert error.value.code == "CATALOG_GENERATION_CHANGED"
    assert client.catalog_generation == 7
    assert len(requests) == 1


def test_read_only_authority_refresh_detects_generation_drift(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"
    thread, requests = _start_server(
        socket_path,
        [
            lambda request: _success(request, _AUTHORITY),
            lambda request: _success(request, {**_AUTHORITY, "catalog_generation": 8}),
        ],
    )
    client = _source_client(socket_path)

    assert client.authority()["catalog_generation"] == 7
    with pytest.raises(PackageRuntimeError) as error:
        client.authority()

    thread.join(timeout=2)
    assert error.value.code == "CATALOG_GENERATION_CHANGED"
    assert client.catalog_generation == 7
    assert [request["operation"] for request in requests] == ["authority", "authority"]


@pytest.mark.parametrize(
    ("response", "error_code"),
    [
        (b"not-json\n", "PACKAGE_RUNTIME_PROTOCOL_INVALID"),
        (
            b'{"request_id":"first","request_id":"second","ok":true,"result":{}}\n',
            "PACKAGE_RUNTIME_PROTOCOL_INVALID",
        ),
        (b"x" * (_MAX_CONTROL_LINE_BYTES + 1), "CONTROL_RESPONSE_TOO_LARGE"),
    ],
)
def test_malformed_and_oversized_responses_fail_closed(tmp_path: Path, response: bytes, error_code: str) -> None:
    socket_path = tmp_path / "control.sock"
    thread, _requests = _start_server(socket_path, [lambda _request: response])

    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path).authority()

    thread.join(timeout=2)
    assert error.value.code == error_code


def test_timeout_fails_closed(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"

    def no_response(_request: dict[str, Any]) -> None:
        time.sleep(0.15)
        return None

    thread, _requests = _start_server(socket_path, [no_response])
    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path, timeout_seconds=0.03).authority()

    thread.join(timeout=2)
    assert error.value.code == "CONTROL_TIMEOUT"


def test_denial_preserves_code_and_redacts_source_and_environment_secrets(
    tmp_path: Path,
) -> None:
    socket_path = tmp_path / "control.sock"

    def responder(request: dict[str, Any]) -> dict[str, Any]:
        if request["operation"] == "authority":
            return _success(request, _AUTHORITY)
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "BINDING_NOT_AUTHORIZED",
                "message": "rejected token=source-secret env=private-env connection_ref=unix:///private/ref",
                "remediation": "check API_TOKEN=private-env",
            },
        }

    thread, requests = _start_server(socket_path, [responder, responder])
    client = _source_client(socket_path)

    with pytest.raises(PackageRuntimeError) as error:
        client.activate("binding-a", "install-a", package_id="package-a", environment={"API_TOKEN": "private-env"})

    thread.join(timeout=2)
    assert error.value.code == "BINDING_NOT_AUTHORIZED"
    assert "source-secret" not in str(error.value)
    assert "private-env" not in str(error.value)
    assert "unix:///private/ref" not in str(error.value)
    assert len(requests) == 2


def test_mutation_is_not_retried_after_lost_response(tmp_path: Path) -> None:
    socket_path = tmp_path / "control.sock"

    def responder(request: dict[str, Any]) -> dict[str, Any] | None:
        if request["operation"] == "authority":
            return _success(request, _AUTHORITY)
        return None

    thread, requests = _start_server(socket_path, [responder, responder])
    with pytest.raises(PackageRuntimeError):
        _source_client(socket_path, timeout_seconds=0.03).activate(
            "binding-a", "install-a", package_id="package-a", environment={"MODE": "training"}
        )

    thread.join(timeout=2)
    assert [request["operation"] for request in requests] == ["authority", "activate"]


def test_oversized_request_is_rejected_before_operation_socket_connect(
    tmp_path: Path,
) -> None:
    socket_path = tmp_path / "control.sock"
    thread, requests = _start_server(socket_path, [lambda request: _success(request, _AUTHORITY)])

    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path).activate(
            "binding-a",
            "install-a",
            package_id="package-a",
            environment={"LARGE_VALUE": "x" * _MAX_CONTROL_LINE_BYTES},
        )

    thread.join(timeout=2)
    assert error.value.code == "CONTROL_REQUEST_TOO_LARGE"
    assert [request["operation"] for request in requests] == ["authority"]


def test_authority_credential_denial_preserves_code_and_redacts_token(
    tmp_path: Path,
) -> None:
    socket_path = tmp_path / "control.sock"

    def deny(request: dict[str, Any]) -> dict[str, Any]:
        return {
            "request_id": request["request_id"],
            "ok": False,
            "error": {
                "code": "SOURCE_CREDENTIAL_INVALID",
                "message": "bad source_token=source-secret",
                "remediation": "replace the source credential",
            },
        }

    thread, _requests = _start_server(socket_path, [deny])
    with pytest.raises(PackageRuntimeError) as error:
        _source_client(socket_path).runtime_status("binding-a")

    thread.join(timeout=2)
    assert error.value.code == "SOURCE_CREDENTIAL_INVALID"
    assert "source-secret" not in str(error.value)


def test_environment_loader_reads_systemd_credential_and_optional_generation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    credential_dir = tmp_path / "credentials"
    credential_dir.mkdir()
    token_file = credential_dir / "activity-token"
    token_file.write_text("from-systemd\n", encoding="utf-8")
    monkeypatch.setenv("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID", "yield-training")
    monkeypatch.setenv("CREDENTIALS_DIRECTORY", str(credential_dir))
    monkeypatch.delenv("CYRENE_RUNTIME_ACTIVITY_SOURCE_TOKEN_FILE", raising=False)
    monkeypatch.delenv("CYRENE_RUNTIME_ACTIVITY_CATALOG_GENERATION", raising=False)
    monkeypatch.setenv("CYRENE_PACKAGE_RUNTIME_SOCKET", str(tmp_path / "control.sock"))

    client = PackageRuntimeClient.from_environment()

    assert client.catalog_generation is None
    assert client._auth_payload() == {
        "source_id": "yield-training",
        "source_token": "from-systemd",
    }


def test_missing_and_invalid_credentials_do_not_select_operator_mode(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    with pytest.raises(ValueError, match="source credentials"):
        PackageRuntimeClient(tmp_path / "control.sock")
    with pytest.raises(ValueError, match="supplied together"):
        PackageRuntimeClient(tmp_path / "control.sock", source_id="yield-training")
    with pytest.raises(ValueError, match="non-empty"):
        PackageRuntimeClient(tmp_path / "control.sock", source_id="yield-training", source_token=" \n")

    monkeypatch.delenv("CYRENE_RUNTIME_ACTIVITY_SOURCE_ID", raising=False)
    with pytest.raises(ValueError, match="SOURCE_ID is required"):
        PackageRuntimeClient.from_environment()

    empty_token = tmp_path / "empty-token"
    empty_token.write_text("\n", encoding="utf-8")
    with pytest.raises(ValueError, match="credential is empty"):
        PackageRuntimeClient.from_source_secret("yield-training", empty_token)


def test_operator_mode_is_explicit_and_requires_root_peer(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("cyrene_runtime_maintenance.package_runtime.os.geteuid", lambda: 1000)

    with pytest.raises(ValueError, match="requires a root process"):
        PackageRuntimeClient(operator=True)


def test_operator_request_uses_empty_auth_only_after_explicit_root_selection(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    socket_path = tmp_path / "control.sock"
    monkeypatch.setattr("cyrene_runtime_maintenance.package_runtime.os.geteuid", lambda: 0)
    thread, requests = _start_server(
        socket_path,
        [
            lambda request: _success(request, _AUTHORITY),
            lambda request: _success(request, {"binding_id": "binding-a", "state": "STOPPED"}),
        ],
    )
    client = PackageRuntimeClient(socket_path, operator=True)

    client.runtime_status("binding-a")

    thread.join(timeout=2)
    assert requests[0]["auth"] == {}
    assert requests[1]["auth"] == {}


def test_rejects_relative_or_broker_socket_paths(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="absolute path"):
        _source_client(Path("relative.sock"))
    with pytest.raises(ValueError, match="own control socket"):
        _source_client(Path("/run/cyrene/runtime-maintenance.sock"))
