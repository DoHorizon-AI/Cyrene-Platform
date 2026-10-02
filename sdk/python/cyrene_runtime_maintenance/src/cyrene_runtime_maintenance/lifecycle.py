"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 lifecycle.py                                                     │
│  Module: cyrene_runtime_maintenance.lifecycle                        │
│  Role: Shared Product task-activity lifecycle coordinator.            │
│                                                                      │
│  模块职责：为Product任务准入、持久化和来源心跳提供统一锁序。            │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable, Iterable, Mapping
from typing import Any, TypeVar

from .client import MaintenanceError, RuntimeMaintenanceClient

T = TypeVar("T")
_NONTERMINAL_STATES = {
    "ACCEPTED",
    "QUEUED",
    "DISPATCHING",
    "RUNNING",
    "CANCELING",
    "INFLIGHT",
}


class ActivitySourceLifecycle:
    """Serializes task reconciliation and Product writes for one activity source.

    Call ``start`` before accepting work. Its startup reconciliation runs before
    the heartbeat thread begins. Every Product task mutation must then go through
    this coordinator so a database snapshot cannot erase a concurrent admission.
    """

    def __init__(
        self,
        client: RuntimeMaintenanceClient,
        *,
        heartbeat_interval_seconds: float = 10.0,
        reconcile_interval_seconds: float = 30.0,
    ) -> None:
        if heartbeat_interval_seconds <= 0 or reconcile_interval_seconds <= 0:
            raise ValueError("heartbeat and reconcile intervals must be positive")
        self._client = client
        self._heartbeat_interval = heartbeat_interval_seconds
        self._reconcile_interval = reconcile_interval_seconds
        self._lock = threading.RLock()
        self._stop_event = threading.Event()
        self._thread: threading.Thread | None = None
        self._load_active_tasks: Callable[[], Iterable[Mapping[str, str]]] | None = None
        self._last_error: MaintenanceError | None = None

    @property
    def last_error(self) -> MaintenanceError | None:
        """Returns the last source-liveness or reconcile error, if one remains."""
        with self._lock:
            return self._last_error

    def start(self, load_active_tasks: Callable[[], Iterable[Mapping[str, str]]]) -> None:
        """Reconciles durable Product state, then starts heartbeat/reconcile work."""
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            self._load_active_tasks = load_active_tasks
            self._reconcile_locked(load_active_tasks)
            self._last_error = None
            self._stop_event.clear()
            self._thread = threading.Thread(
                target=self._background_loop,
                name="cyrene-runtime-activity-heartbeat",
                daemon=True,
            )
            self._thread.start()

    def reconcile(self, load_active_tasks: Callable[[], Iterable[Mapping[str, str]]] | None = None) -> None:
        """Refreshes the gate task set under the same local lock as task writes."""
        with self._lock:
            loader = load_active_tasks or self._load_active_tasks
            if loader is None:
                raise MaintenanceError("ACTIVITY_SOURCE_NOT_READY", "source lifecycle has not started")
            self._reconcile_locked(loader)

    def admit_and_persist(
        self,
        task_id: str,
        persist: Callable[[], T],
        *,
        state: str = "ACCEPTED",
    ) -> T:
        """Reserves activity before committing a newly accepted Product task."""
        _validate_state(state)
        with self._lock:
            self._require_started_and_healthy()
            self._client.admit_task(task_id, state=state)
            try:
                return persist()
            except Exception as persist_error:
                try:
                    self._client.complete_task(task_id)
                except MaintenanceError as cleanup_error:
                    self._last_error = cleanup_error
                    raise cleanup_error from persist_error
                raise

    def transition_and_persist(self, task_id: str, state: str, persist: Callable[[], T]) -> T:
        """Records a nonterminal gate state before committing the Product transition."""
        _validate_state(state)
        with self._lock:
            self._require_started_and_healthy()
            self._client.update_task_activity(task_id, state=state)
            return persist()

    def complete_after_persist(self, task_id: str, persist: Callable[[], T]) -> T:
        """Commits a terminal Product state before removing its active gate record."""
        with self._lock:
            value = persist()
            try:
                self._client.complete_task(task_id)
            except MaintenanceError as error:
                # Terminal domain state must not be held hostage by a broker
                # outage. Its gate record stays active until a later
                # reconciliation can prove that the Product DB is terminal.
                self._last_error = error
                raise
            return value

    def close(self, timeout_seconds: float = 5.0) -> None:
        """Stops the source heartbeat thread during orderly service shutdown."""
        self._stop_event.set()
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout_seconds)
        self._thread = None

    def _reconcile_locked(
        self, load_active_tasks: Callable[[], Iterable[Mapping[str, str]]]
    ) -> None:
        tasks = []
        for task in load_active_tasks():
            task_id = task.get("task_id")
            state = task.get("state")
            if not isinstance(task_id, str) or not task_id:
                raise ValueError("active task records need a non-empty task_id")
            if not isinstance(state, str):
                raise ValueError(f"active task {task_id} needs a string state")
            normalized_state = state.upper()
            _validate_state(normalized_state)
            tasks.append({"task_id": task_id, "state": normalized_state})
        self._client.reconcile_activity_source(tasks)
        self._last_error = None

    def _require_started_and_healthy(self) -> None:
        if self._thread is None or not self._thread.is_alive():
            raise MaintenanceError("ACTIVITY_SOURCE_NOT_READY", "source lifecycle has not started")
        if self._last_error is not None:
            raise self._last_error

    def _background_loop(self) -> None:
        next_heartbeat = time.monotonic() + self._heartbeat_interval
        next_reconcile = time.monotonic() + self._reconcile_interval
        while not self._stop_event.is_set():
            deadline = min(next_heartbeat, next_reconcile)
            if self._stop_event.wait(max(0.0, deadline - time.monotonic())):
                return
            try:
                with self._lock:
                    if time.monotonic() >= next_reconcile:
                        loader = self._load_active_tasks
                        if loader is None:
                            raise MaintenanceError(
                                "ACTIVITY_SOURCE_NOT_READY", "active-task loader is unavailable"
                            )
                        self._reconcile_locked(loader)
                        next_reconcile = time.monotonic() + self._reconcile_interval
                        next_heartbeat = time.monotonic() + self._heartbeat_interval
                    else:
                        self._client.heartbeat_activity_source()
                        self._last_error = None
                        next_heartbeat = time.monotonic() + self._heartbeat_interval
            except MaintenanceError as error:
                with self._lock:
                    self._last_error = error
            except Exception as error:
                with self._lock:
                    self._last_error = MaintenanceError(
                        "ACTIVITY_SOURCE_UNKNOWN", str(error), readiness_status="UNKNOWN"
                    )


def _validate_state(state: str) -> None:
    if state not in _NONTERMINAL_STATES:
        raise ValueError(f"unsupported active task state: {state}")
