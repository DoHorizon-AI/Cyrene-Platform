"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 test_lifecycle.py                                                 │
│  Module: cyrene_runtime_maintenance.tests.test_lifecycle              │
│  Role: Product task lifecycle ordering tests.                          │
│                                                                      │
│  模块职责：验证Product持久任务状态与共享门禁调用的锁序。                 │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

from typing import Any

import pytest

from cyrene_runtime_maintenance import ActivitySourceLifecycle, MaintenanceError


class _FakeClient:
    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.fail_complete = False

    def reconcile_activity_source(self, tasks: Any) -> dict[str, Any]:
        self.calls.append(("reconcile", tasks))
        return {"reconciled": True}

    def heartbeat_activity_source(self) -> dict[str, Any]:
        self.calls.append(("heartbeat", None))
        return {}

    def admit_task(self, task_id: str, *, state: str) -> dict[str, Any]:
        self.calls.append(("admit", (task_id, state)))
        return {"admitted": True}

    def update_task_activity(self, task_id: str, *, state: str) -> dict[str, Any]:
        self.calls.append(("update", (task_id, state)))
        return {"accepted": True}

    def complete_task(self, task_id: str) -> dict[str, Any]:
        self.calls.append(("complete", task_id))
        if self.fail_complete:
            raise MaintenanceError("UPDATE_READINESS_UNKNOWN", "broker disconnected")
        return {"completed": True}


def test_start_reconciles_before_task_acceptance_and_close_stops_thread() -> None:
    client = _FakeClient()
    lifecycle = ActivitySourceLifecycle(
        client,
        heartbeat_interval_seconds=60,
        reconcile_interval_seconds=120,
    )
    lifecycle.start(lambda: [{"task_id": "run-1", "state": "running"}])
    persisted: list[str] = []

    result = lifecycle.admit_and_persist(
        "run-2", lambda: persisted.append("commit") or "saved"
    )
    lifecycle.close()

    assert result == "saved"
    assert persisted == ["commit"]
    assert client.calls[0] == (
        "reconcile",
        [{"task_id": "run-1", "state": "RUNNING"}],
    )
    assert client.calls[1] == ("admit", ("run-2", "ACCEPTED"))


def test_failed_domain_acceptance_removes_gate_record() -> None:
    client = _FakeClient()
    lifecycle = ActivitySourceLifecycle(client, heartbeat_interval_seconds=60)
    lifecycle.start(lambda: [])
    error = RuntimeError("database write failed")

    with pytest.raises(RuntimeError, match="database write failed"):
        lifecycle.admit_and_persist("run-3", lambda: (_raise(error)))

    lifecycle.close()
    assert [name for name, _ in client.calls] == ["reconcile", "admit", "complete"]


def test_invalid_active_state_fails_before_gate_call() -> None:
    client = _FakeClient()
    lifecycle = ActivitySourceLifecycle(client, heartbeat_interval_seconds=60)
    lifecycle.start(lambda: [])

    with pytest.raises(ValueError, match="unsupported active task state"):
        lifecycle.transition_and_persist("run-4", "SUCCEEDED", lambda: None)

    lifecycle.close()
    assert [name for name, _ in client.calls] == ["reconcile"]


def test_terminal_domain_write_is_not_blocked_by_broker_failure() -> None:
    client = _FakeClient()
    lifecycle = ActivitySourceLifecycle(client, heartbeat_interval_seconds=60)
    lifecycle.start(lambda: [{"task_id": "run-5", "state": "RUNNING"}])
    client.fail_complete = True
    persisted: list[str] = []

    with pytest.raises(MaintenanceError, match="broker disconnected"):
        lifecycle.complete_after_persist("run-5", lambda: persisted.append("terminal"))

    lifecycle.close()
    assert persisted == ["terminal"]
    assert lifecycle.last_error is not None


def _raise(error: Exception) -> None:
    raise error
