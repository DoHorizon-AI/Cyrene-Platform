import pytest

from cyrene_control_plane import AttemptStatus, ExecutionPlan, IdempotencyKey, JsonFileControlPlaneStore, PlanStatus, PlanStep, ProductControlPlane, ReconcileActionKind, RetryPolicy, StaleGenerationError, StepDependency


def _plan(*, retry=RetryPolicy(max_attempts=2)):
    return ExecutionPlan.create([
        PlanStep(step_id="prepare", capability="capability.prepare", retry_policy=retry),
        PlanStep(step_id="execute", capability="capability.execute", dependencies=(StepDependency("prepare"),), retry_policy=retry),
    ], metadata={"intent_digest": "intent-1"})


def _control(tmp_path, plan=None):
    store = JsonFileControlPlaneStore(tmp_path / "control-plane.json")
    control = ProductControlPlane(store)
    run = control.submit(plan or _plan(), product_kind="example.product", idempotency_key=IdempotencyKey("submit-1"))
    return store, control, run


def test_same_idempotency_key_returns_one_logical_product_run(tmp_path):
    _, control, run = _control(tmp_path)
    duplicate = control.submit(_plan(), product_kind="example.product", idempotency_key=IdempotencyKey("submit-1"))
    assert duplicate.run_id == run.run_id
    assert duplicate.plan_id == run.plan_id


def test_plan_identity_and_dependency_order_are_deterministic():
    assert _plan().plan_id == _plan().plan_id
    assert [step.step_id for step in _plan().ordered_steps()] == ["prepare", "execute"]


def test_retry_creates_new_immutable_attempt(tmp_path):
    _, control, run = _control(tmp_path)
    first = control.start_next_attempt(run.run_id)
    updated = control.observe_attempt(run.run_id, first.attempt_id, AttemptStatus.FAILED, error="transient")
    assert control.next_action(updated.run_id).kind is ReconcileActionKind.RETRY_ATTEMPT
    second = control.start_next_attempt(updated.run_id)
    assert second.attempt_id != first.attempt_id
    assert control.load(run.run_id).attempts[0].status is AttemptStatus.FAILED
    with pytest.raises(ValueError, match="immutable"):
        control.observe_attempt(run.run_id, first.attempt_id, AttemptStatus.RUNNING)


def test_stale_generation_cannot_overwrite_newer_observation(tmp_path):
    store, control, run = _control(tmp_path)
    attempt = control.start_next_attempt(run.run_id)
    stale = control.load(run.run_id)
    current = control.load(run.run_id)
    observed = current.with_observed_attempt(attempt.with_observation(AttemptStatus.RUNNING))
    store.compare_and_set(observed, current.generation)
    with pytest.raises(StaleGenerationError):
        store.compare_and_set(stale, stale.generation)


def test_restart_restores_pending_and_running_state(tmp_path):
    store, control, run = _control(tmp_path)
    attempt = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, attempt.attempt_id, AttemptStatus.RUNNING)
    restarted = JsonFileControlPlaneStore(store.path)
    restored = restarted.load_run(run.run_id)
    assert restored.observed_status is PlanStatus.RUNNING
    assert restored.latest_attempt.status is AttemptStatus.RUNNING
    assert [item.run_id for item in restarted.list_nonterminal()] == [run.run_id]


def test_failed_or_lost_attempt_retries_then_exhaustion_fails(tmp_path):
    _, control, run = _control(tmp_path, _plan(retry=RetryPolicy(max_attempts=2)))
    first = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, first.attempt_id, AttemptStatus.LOST)
    assert control.next_action(run.run_id).kind is ReconcileActionKind.RETRY_ATTEMPT
    second = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, second.attempt_id, AttemptStatus.FAILED)
    assert control.next_action(run.run_id).kind is ReconcileActionKind.FINALIZE_FAILED
    assert control.apply_terminal_action(run.run_id).observed_status is PlanStatus.FAILED


def test_cancellation_waits_for_cleanup_confirmation(tmp_path):
    _, control, run = _control(tmp_path)
    attempt = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, attempt.attempt_id, AttemptStatus.RUNNING)
    control.request_cancel(run.run_id)
    assert control.next_action(run.run_id).kind is ReconcileActionKind.REQUEST_CANCELLATION
    control.observe_attempt(run.run_id, attempt.attempt_id, AttemptStatus.CANCELLING, cleanup_confirmed=False)
    assert control.load(run.run_id).observed_status is PlanStatus.CANCELLING
    control.observe_attempt(run.run_id, attempt.attempt_id, AttemptStatus.CANCELLED, cleanup_confirmed=True)
    assert control.next_action(run.run_id).kind is ReconcileActionKind.FINALIZE_CANCELLED
    assert control.apply_terminal_action(run.run_id).observed_status is PlanStatus.CANCELLED


def test_completed_run_is_not_duplicated_after_restart(tmp_path):
    store, control, run = _control(tmp_path)
    first = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, first.attempt_id, AttemptStatus.SUCCEEDED)
    second = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, second.attempt_id, AttemptStatus.SUCCEEDED)
    completed = control.apply_terminal_action(run.run_id)
    assert completed.observed_status is PlanStatus.SUCCEEDED
    restarted = ProductControlPlane(JsonFileControlPlaneStore(store.path))
    duplicate = restarted.submit(_plan(), product_kind="example.product", idempotency_key=IdempotencyKey("submit-1"))
    assert duplicate.run_id == run.run_id
    assert duplicate.observed_status is PlanStatus.SUCCEEDED


def test_blocked_step_prevents_plan_advancement(tmp_path):
    _, control, run = _control(tmp_path)
    attempt = control.start_next_attempt(run.run_id)
    control.observe_attempt(run.run_id, attempt.attempt_id, AttemptStatus.BLOCKED)
    assert control.next_action(run.run_id).kind is ReconcileActionKind.FINALIZE_BLOCKED
    assert control.apply_terminal_action(run.run_id).observed_status is PlanStatus.BLOCKED
