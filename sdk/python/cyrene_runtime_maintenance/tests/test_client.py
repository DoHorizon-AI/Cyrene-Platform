"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 test_client.py                                                   │
│  Module: cyrene_runtime_maintenance.tests.test_client                │
│  Role: SDK protocol and fail-closed behavior tests.                   │
│                                                                      │
│  模块职责：验证SDK JSON协议与不可达时的fail-closed行为。                │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import socket
import threading
from pathlib import Path
from typing import Any

import pytest

from cyrene_runtime_maintenance import MaintenanceError, RuntimeMaintenanceClient


def _one_request_server(
    socket_path: Path, result: dict[str, Any]
) -> tuple[threading.Thread, list[dict[str, Any]]]:
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    listener.listen(1)
    requests: list[dict[str, Any]] = []

    def serve_once() -> None:
        with listener:
            connection, _ = listener.accept()
            with connection:
                stream = connection.makefile("rb")
                request = json.loads(stream.readline())
                requests.append(request)
                response = {"request_id": request["request_id"], "result": result}
                connection.sendall(json.dumps(response).encode() + b"\n")

    thread = threading.Thread(target=serve_once, daemon=True)
    thread.start()
    return thread, requests


def test_source_request_keeps_token_in_auth_and_reconciles_shape(tmp_path: Path) -> None:
    socket_path = tmp_path / "runtime-maintenance.sock"
    thread, requests = _one_request_server(
        socket_path,
        {"reconciled": True, "gate_generation": 4, "catalog_generation": 8},
    )
    client = RuntimeMaintenanceClient(
        socket_path,
        source_id="cyrene-yield",
        source_token="source-secret",
        catalog_generation=3,
    )

    result = client.reconcile_activity_source(
        [{"task_id": "training-7", "state": "RUNNING"}]
    )

    thread.join(timeout=2)
    assert result["reconciled"] is True
    assert requests[0]["auth"] == {
        "source_id": "cyrene-yield",
        "source_token": "source-secret",
    }
    assert requests[0]["params"] == {
        "source_id": "cyrene-yield",
        "expected_catalog_generation": 3,
        "active_tasks": [{"task_id": "training-7", "state": "RUNNING"}],
    }
    assert client.catalog_generation == 8


def test_unavailable_broker_is_unknown(tmp_path: Path) -> None:
    client = RuntimeMaintenanceClient(
        tmp_path / "missing.sock",
        source_id="cyrene-yield",
        source_token="source-secret",
        catalog_generation=3,
        timeout_seconds=0.1,
    )

    with pytest.raises(MaintenanceError) as error:
        client.heartbeat_activity_source()

    assert error.value.code == "UPDATE_READINESS_UNKNOWN"
    assert error.value.readiness_status == "UNKNOWN"


def test_begin_requires_canonical_plan_digest_before_ipc(tmp_path: Path) -> None:
    client = RuntimeMaintenanceClient.as_operator(
        _write_secret(tmp_path / "operator.token"), catalog_generation=3
    )

    with pytest.raises(ValueError, match="sha256:"):
        client.begin_maintenance(
            "apply-1",
            expected_gate_generation=4,
            expected_activity_sources=["cyrene-yield"],
            user_confirmed_restart=True,
            plan_id="plan-1",
            plan_digest="a" * 64,
            component_artifact_digests={"cyrene-yield": f"sha256:{'b' * 64}"},
        )


def _write_secret(path: Path) -> Path:
    path.write_text("operator-secret\n", encoding="utf-8")
    return path
