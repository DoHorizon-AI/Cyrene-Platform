# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_control_plane/src/cyrene_control_plane/reconciler.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Generic desired-versus-observed Product reconciliation."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Optional, Sequence

from .contracts import Attempt, AttemptId, AttemptNumber, AttemptStatus, DesiredState, ExecutionPlan, Generation, IdempotencyKey, PlanStatus, ProductRun, StepStatus
from .store import ControlPlaneStore


class ReconcileActionKind(str, Enum):
    CREATE_ATTEMPT = "create_attempt"
    RETRY_ATTEMPT = "retry_attempt"
    WAIT = "wait"
    REQUEST_CANCELLATION = "request_cancellation"
    FINALIZE_SUCCEEDED = "finalize_succeeded"
    FINALIZE_FAILED = "finalize_failed"
    FINALIZE_BLOCKED = "finalize_blocked"
    FINALIZE_CANCELLED = "finalize_cancelled"
    NOOP = "noop"


@dataclass(frozen=True)
class ReconcileAction:
    kind: ReconcileActionKind
    run_id: str
    step_id: Optional[str] = None
    attempt_id: Optional[AttemptId] = None
    reason: str = ""


class ProductReconciler:
    """Pure policy: compare desired state to observations and choose one next action."""

    def next_action(self, plan: ExecutionPlan, run: ProductRun) -> ReconcileAction:
        if run.terminal:
            return ReconcileAction(ReconcileActionKind.NOOP, run.run_id, reason="ProductRun is terminal")
        latest = run.latest_attempt
        if run.desired_state is DesiredState.CANCELLED:
            if latest is not None and latest.status in {AttemptStatus.PENDING, AttemptStatus.RUNNING, AttemptStatus.CANCELLING}:
                return ReconcileAction(ReconcileActionKind.REQUEST_CANCELLATION, run.run_id, latest.step_id, latest.attempt_id, "Cancellation is desired; execution cleanup is not confirmed")
            if latest is not None and latest.status is AttemptStatus.CANCELLED and not latest.cleanup_confirmed:
                return ReconcileAction(ReconcileActionKind.WAIT, run.run_id, latest.step_id, latest.attempt_id, "Cancellation acknowledged but cleanup remains unconfirmed")
            return ReconcileAction(ReconcileActionKind.FINALIZE_CANCELLED, run.run_id)
        if any(status is StepStatus.BLOCKED for status in run.step_statuses.values()):
            return ReconcileAction(ReconcileActionKind.FINALIZE_BLOCKED, run.run_id)
        if latest is not None:
            if latest.status in {AttemptStatus.PENDING, AttemptStatus.RUNNING, AttemptStatus.CANCELLING}:
                return ReconcileAction(ReconcileActionKind.WAIT, run.run_id, latest.step_id, latest.attempt_id, "Concrete Attempt is still active")
            if latest.status in {AttemptStatus.FAILED, AttemptStatus.LOST}:
                step = plan.step(latest.step_id)
                count = len(run.attempts_for_step(latest.step_id))
                if step.retry_policy.allows(latest.status, count):
                    return ReconcileAction(ReconcileActionKind.RETRY_ATTEMPT, run.run_id, latest.step_id, latest.attempt_id, f"{latest.status.value} attempt has retry capacity")
                return ReconcileAction(ReconcileActionKind.FINALIZE_FAILED, run.run_id, latest.step_id, latest.attempt_id, "Retry policy is exhausted")
            if latest.status is AttemptStatus.BLOCKED:
                return ReconcileAction(ReconcileActionKind.FINALIZE_BLOCKED, run.run_id, latest.step_id, latest.attempt_id)
        step = self._next_runnable_step(plan, run)
        if step is not None:
            return ReconcileAction(ReconcileActionKind.CREATE_ATTEMPT, run.run_id, step.step_id, reason="Dependency ordering permits the next PlanStep")
        if all(run.step_statuses.get(step.step_id) is StepStatus.SUCCEEDED for step in plan.steps):
            return ReconcileAction(ReconcileActionKind.FINALIZE_SUCCEEDED, run.run_id)
        return ReconcileAction(ReconcileActionKind.WAIT, run.run_id, reason="Waiting for external observation")

    @staticmethod
    def _next_runnable_step(plan: ExecutionPlan, run: ProductRun):
        for step in plan.ordered_steps():
            status = run.step_statuses.get(step.step_id, StepStatus.PENDING)
            if status is not StepStatus.PENDING:
                continue
            if all(run.step_statuses.get(dependency.step_id, StepStatus.PENDING) is dependency.required_status for dependency in step.dependencies):
                return step
        return None


class ProductControlPlane:
    """Small application service over the persistence port and pure reconciler."""

    def __init__(self, store: ControlPlaneStore, reconciler: Optional[ProductReconciler] = None) -> None:
        self._store = store
        self._reconciler = reconciler or ProductReconciler()

    def submit(self, plan: ExecutionPlan, *, product_kind: str, idempotency_key: IdempotencyKey, metadata: Optional[dict] = None) -> ProductRun:
        return self._store.create_or_get(plan, product_kind=product_kind, idempotency_key=idempotency_key, metadata=metadata)

    def load(self, run_id: str) -> ProductRun:
        return self._store.load_run(run_id)

    def next_action(self, run_id: str) -> ReconcileAction:
        run = self._store.load_run(run_id)
        return self._reconciler.next_action(self._store.load_plan(run.plan_id), run)

    def start_next_attempt(self, run_id: str) -> Attempt:
        run = self._store.load_run(run_id)
        action = self._reconciler.next_action(self._store.load_plan(run.plan_id), run)
        if action.kind not in {ReconcileActionKind.CREATE_ATTEMPT, ReconcileActionKind.RETRY_ATTEMPT}:
            raise ValueError(f"ProductRun {run_id} cannot create an Attempt: {action.kind.value}")
        assert action.step_id is not None
        attempt_number = AttemptNumber(len(run.attempts) + 1)
        attempt = Attempt(AttemptId(f"{run.run_id}:{int(attempt_number)}"), run.run_id, attempt_number, action.step_id)
        updated = run.with_attempt(attempt).with_step_status(action.step_id, StepStatus.RUNNING).with_observed_status(PlanStatus.RUNNING)
        self._store.compare_and_set(updated, run.generation)
        return attempt

    def observe_attempt(self, run_id: str, attempt_id: AttemptId, status: AttemptStatus, *, execution_references: Optional[Sequence[str]] = None, cleanup_confirmed: Optional[bool] = None, error: Optional[str] = None, expected_generation: Optional[Generation] = None) -> ProductRun:
        run = self._store.load_run(run_id)
        if expected_generation is not None and run.generation != expected_generation:
            from .store import StaleGenerationError
            raise StaleGenerationError(f"ProductRun {run_id} is generation {run.generation}, not {expected_generation}")
        attempt = next((item for item in run.attempts if item.attempt_id == attempt_id), None)
        if attempt is None:
            raise KeyError(f"Unknown Attempt: {attempt_id}")
        observed = attempt.with_observation(status, execution_references=execution_references, cleanup_confirmed=cleanup_confirmed, error=error)
        updated = run.with_observed_attempt(observed)
        if status in {AttemptStatus.FAILED, AttemptStatus.LOST}:
            updated = updated.with_observed_status(PlanStatus.AWAITING_RETRY)
        elif status is AttemptStatus.CANCELLING:
            updated = updated.with_observed_status(PlanStatus.CANCELLING)
        elif status is AttemptStatus.CANCELLED:
            updated = updated.with_observed_status(PlanStatus.CANCEL_REQUESTED)
        return self._store.compare_and_set(updated, run.generation)

    def request_cancel(self, run_id: str) -> ProductRun:
        run = self._store.load_run(run_id)
        if run.terminal:
            return run
        updated = run.with_desired_state(DesiredState.CANCELLED).with_observed_status(PlanStatus.CANCEL_REQUESTED)
        return self._store.compare_and_set(updated, run.generation)

    def apply_terminal_action(self, run_id: str) -> ProductRun:
        run = self._store.load_run(run_id)
        action = self._reconciler.next_action(self._store.load_plan(run.plan_id), run)
        transitions = {ReconcileActionKind.FINALIZE_SUCCEEDED: PlanStatus.SUCCEEDED, ReconcileActionKind.FINALIZE_FAILED: PlanStatus.FAILED, ReconcileActionKind.FINALIZE_BLOCKED: PlanStatus.BLOCKED, ReconcileActionKind.FINALIZE_CANCELLED: PlanStatus.CANCELLED}
        target = transitions.get(action.kind)
        if target is None:
            return run
        return self._store.compare_and_set(run.with_observed_status(target), run.generation)


__all__ = ["ProductControlPlane", "ProductReconciler", "ReconcileAction", "ReconcileActionKind"]
