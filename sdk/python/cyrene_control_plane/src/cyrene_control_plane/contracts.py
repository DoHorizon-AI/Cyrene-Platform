"""Stable, product-neutral Product Control Plane contracts.

These types live above Kernel Operations. A Product compiles intent into an
ExecutionPlan, while each PlanStep can later be realized by generic Kernel
Operations.
"""

from __future__ import annotations

import hashlib
import json
import uuid
from dataclasses import dataclass, field, replace
from datetime import datetime, timezone
from enum import Enum
from typing import Any, Dict, Mapping, NewType, Optional, Sequence, Tuple

from cy_artifacts import ArtifactRef

CONTRACT_VERSION = "cyrene.control-plane.v1"


AttemptId = NewType("AttemptId", str)
AttemptNumber = NewType("AttemptNumber", int)


# Invariant: Generation is a strictly monotonic counter incremented by the Reconciler
# upon each state change or retry. It prevents stale asynchronous completion events from
# a previous Attempt or zombie worker from overwriting the observed status of a newer Attempt.
Generation = NewType("Generation", int)

# Invariant: IdempotencyKey ensures that re-submitted Product intent produces the exact
# same ExecutionPlan without creating duplicate concurrent runs or leaked resource leases.
IdempotencyKey = NewType("IdempotencyKey", str)



class StepStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    BLOCKED = "blocked"
    CANCELLING = "cancelling"
    CANCELLED = "cancelled"


class PlanStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    AWAITING_RETRY = "awaiting_retry"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    BLOCKED = "blocked"
    CANCEL_REQUESTED = "cancel_requested"
    CANCELLING = "cancelling"
    CANCELLED = "cancelled"


class DesiredState(str, Enum):
    ACTIVE = "active"
    CANCELLED = "cancelled"


class AttemptStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    LOST = "lost"
    BLOCKED = "blocked"
    CANCELLING = "cancelling"
    CANCELLED = "cancelled"

    @property
    def terminal(self) -> bool:
        return self in {
            AttemptStatus.SUCCEEDED,
            AttemptStatus.FAILED,
            AttemptStatus.LOST,
            AttemptStatus.BLOCKED,
            AttemptStatus.CANCELLED,
        }


@dataclass(frozen=True)
class StepDependency:
    step_id: str
    required_status: StepStatus = StepStatus.SUCCEEDED

    def to_dict(self) -> Dict[str, str]:
        return {"step_id": self.step_id, "required_status": self.required_status.value}

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "StepDependency":
        return cls(
            step_id=str(data["step_id"]),
            required_status=StepStatus(str(data.get("required_status", StepStatus.SUCCEEDED.value))),
        )


@dataclass(frozen=True)
class RetryPolicy:
    """Retry bounds are per generic plan step, with the first try included."""

    max_attempts: int = 1
    retry_failed: bool = True
    retry_lost: bool = True

    def __post_init__(self) -> None:
        if self.max_attempts < 1:
            raise ValueError("RetryPolicy.max_attempts must be at least one")

    def allows(self, status: AttemptStatus, attempts_so_far: int) -> bool:
        if attempts_so_far >= self.max_attempts:
            return False
        if status is AttemptStatus.FAILED:
            return self.retry_failed
        if status is AttemptStatus.LOST:
            return self.retry_lost
        return False

    def to_dict(self) -> Dict[str, Any]:
        return {
            "max_attempts": self.max_attempts,
            "retry_failed": self.retry_failed,
            "retry_lost": self.retry_lost,
        }

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "RetryPolicy":
        return cls(
            max_attempts=int(data.get("max_attempts", 1)),
            retry_failed=bool(data.get("retry_failed", True)),
            retry_lost=bool(data.get("retry_lost", True)),
        )


@dataclass(frozen=True)
class PlanStep:
    """One product-neutral capability realization in an ExecutionPlan."""

    step_id: str
    capability: str
    inputs: Tuple[ArtifactRef, ...] = ()
    outputs: Tuple[str, ...] = ()
    environment_identity: Optional[str] = None
    resource_reference: Optional[str] = None
    dependencies: Tuple[StepDependency, ...] = ()
    retry_policy: RetryPolicy = field(default_factory=RetryPolicy)
    execution_payload_ref: Optional[str] = None
    metadata: Mapping[str, Any] = field(default_factory=dict)
    provenance: Mapping[str, Any] = field(default_factory=dict)
    status: StepStatus = StepStatus.PENDING

    def __post_init__(self) -> None:
        if not self.step_id:
            raise ValueError("PlanStep.step_id is required")
        if not self.capability:
            raise ValueError("PlanStep.capability is required")
        if any(item.step_id == self.step_id for item in self.dependencies):
            raise ValueError("A PlanStep cannot depend on itself")

    def identity_dict(self) -> Dict[str, Any]:
        return {
            "step_id": self.step_id,
            "capability": self.capability,
            "inputs": [_artifact_dict(item) for item in self.inputs],
            "outputs": list(self.outputs),
            "environment_identity": self.environment_identity,
            "resource_reference": self.resource_reference,
            "dependencies": [item.to_dict() for item in self.dependencies],
            "retry_policy": self.retry_policy.to_dict(),
            "execution_payload_ref": self.execution_payload_ref,
            "metadata": _normalized(self.metadata),
            "provenance": _normalized(self.provenance),
        }

    def to_dict(self) -> Dict[str, Any]:
        value = self.identity_dict()
        value["status"] = self.status.value
        return value

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "PlanStep":
        return cls(
            step_id=str(data["step_id"]),
            capability=str(data["capability"]),
            inputs=tuple(ArtifactRef.from_dict(item) for item in data.get("inputs", ())),
            outputs=tuple(str(item) for item in data.get("outputs", ())),
            environment_identity=_optional_string(data.get("environment_identity")),
            resource_reference=_optional_string(data.get("resource_reference")),
            dependencies=tuple(StepDependency.from_dict(item) for item in data.get("dependencies", ())),
            retry_policy=RetryPolicy.from_dict(data.get("retry_policy", {})),
            execution_payload_ref=_optional_string(data.get("execution_payload_ref")),
            metadata=dict(data.get("metadata", {})),
            provenance=dict(data.get("provenance", {})),
            status=StepStatus(str(data.get("status", StepStatus.PENDING.value))),
        )


@dataclass(frozen=True)
class ExecutionPlan:
    """Deterministic realization of resolved Product intent through capabilities."""

    plan_id: str
    steps: Tuple[PlanStep, ...]
    contract_version: str = CONTRACT_VERSION
    metadata: Mapping[str, Any] = field(default_factory=dict)
    provenance: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.plan_id:
            raise ValueError("ExecutionPlan.plan_id is required")
        identifiers = [step.step_id for step in self.steps]
        if len(identifiers) != len(set(identifiers)):
            raise ValueError("ExecutionPlan step identities must be unique")
        known = set(identifiers)
        missing = sorted(
            dependency.step_id
            for step in self.steps
            for dependency in step.dependencies
            if dependency.step_id not in known
        )
        if missing:
            raise ValueError(f"ExecutionPlan has missing step dependencies: {missing}")
        self.ordered_steps()

    @classmethod
    def create(
        cls,
        steps: Sequence[PlanStep],
        *,
        metadata: Optional[Mapping[str, Any]] = None,
        provenance: Optional[Mapping[str, Any]] = None,
        contract_version: str = CONTRACT_VERSION,
    ) -> "ExecutionPlan":
        candidate = {
            "contract_version": contract_version,
            "steps": [step.identity_dict() for step in steps],
            "metadata": _normalized(metadata or {}),
            "provenance": _normalized(provenance or {}),
        }
        canonical = json.dumps(candidate, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        plan_id = f"plan-{hashlib.sha256(canonical.encode('utf-8')).hexdigest()}"
        return cls(
            plan_id=plan_id,
            steps=tuple(steps),
            contract_version=contract_version,
            metadata=dict(metadata or {}),
            provenance=dict(provenance or {}),
        )

    def ordered_steps(self) -> Tuple[PlanStep, ...]:
        """Return a deterministic topological order while preserving declared order."""

        remaining = {step.step_id: step for step in self.steps}
        completed = set()
        ordered = []
        while remaining:
            ready = [
                step
                for step in self.steps
                if step.step_id in remaining
                and all(dependency.step_id in completed for dependency in step.dependencies)
            ]
            if not ready:
                raise ValueError("ExecutionPlan dependencies must be acyclic")
            for step in ready:
                ordered.append(step)
                completed.add(step.step_id)
                del remaining[step.step_id]
        return tuple(ordered)

    def step(self, step_id: str) -> PlanStep:
        for item in self.steps:
            if item.step_id == step_id:
                return item
        raise KeyError(f"Unknown plan step: {step_id}")

    def identity_dict(self) -> Dict[str, Any]:
        return {
            "contract_version": self.contract_version,
            "steps": [step.identity_dict() for step in self.steps],
            "metadata": _normalized(self.metadata),
            "provenance": _normalized(self.provenance),
        }

    def to_dict(self) -> Dict[str, Any]:
        value = self.identity_dict()
        value["plan_id"] = self.plan_id
        value["steps"] = [step.to_dict() for step in self.steps]
        return value

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "ExecutionPlan":
        return cls(
            plan_id=str(data["plan_id"]),
            steps=tuple(PlanStep.from_dict(item) for item in data.get("steps", ())),
            contract_version=str(data.get("contract_version", CONTRACT_VERSION)),
            metadata=dict(data.get("metadata", {})),
            provenance=dict(data.get("provenance", {})),
        )


@dataclass(frozen=True)
class Attempt:
    attempt_id: AttemptId
    run_id: str
    number: AttemptNumber
    step_id: str
    status: AttemptStatus = AttemptStatus.PENDING
    execution_references: Tuple[str, ...] = ()
    cleanup_confirmed: bool = False
    error: Optional[str] = None
    metadata: Mapping[str, Any] = field(default_factory=dict)
    created_at: str = field(default_factory=lambda: _timestamp())
    observed_at: str = field(default_factory=lambda: _timestamp())

    def with_observation(
        self,
        status: AttemptStatus,
        *,
        execution_references: Optional[Sequence[str]] = None,
        cleanup_confirmed: Optional[bool] = None,
        error: Optional[str] = None,
    ) -> "Attempt":
        if self.status.terminal and status is not self.status:
            raise ValueError(f"Terminal Attempt {self.attempt_id} is immutable")
        return replace(
            self,
            status=status,
            execution_references=(
                self.execution_references
                if execution_references is None
                else tuple(str(item) for item in execution_references)
            ),
            cleanup_confirmed=self.cleanup_confirmed if cleanup_confirmed is None else cleanup_confirmed,
            error=self.error if error is None else error,
            observed_at=_timestamp(),
        )

    def to_dict(self) -> Dict[str, Any]:
        return {
            "attempt_id": str(self.attempt_id),
            "run_id": self.run_id,
            "number": int(self.number),
            "step_id": self.step_id,
            "status": self.status.value,
            "execution_references": list(self.execution_references),
            "cleanup_confirmed": self.cleanup_confirmed,
            "error": self.error,
            "metadata": _normalized(self.metadata),
            "created_at": self.created_at,
            "observed_at": self.observed_at,
        }

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Attempt":
        return cls(
            attempt_id=AttemptId(str(data["attempt_id"])),
            run_id=str(data["run_id"]),
            number=AttemptNumber(int(data["number"])),
            step_id=str(data["step_id"]),
            status=AttemptStatus(str(data.get("status", AttemptStatus.PENDING.value))),
            execution_references=tuple(str(item) for item in data.get("execution_references", ())),
            cleanup_confirmed=bool(data.get("cleanup_confirmed", False)),
            error=_optional_string(data.get("error")),
            metadata=dict(data.get("metadata", {})),
            created_at=str(data.get("created_at", _timestamp())),
            observed_at=str(data.get("observed_at", _timestamp())),
        )


@dataclass(frozen=True)
class ProductRun:
    """Desired and observed state for one Product intent realization."""

    run_id: str
    product_kind: str
    plan_id: str
    idempotency_key: IdempotencyKey
    desired_state: DesiredState = DesiredState.ACTIVE
    observed_status: PlanStatus = PlanStatus.PENDING
    generation: Generation = Generation(0)
    attempts: Tuple[Attempt, ...] = ()
    step_statuses: Mapping[str, StepStatus] = field(default_factory=dict)
    metadata: Mapping[str, Any] = field(default_factory=dict)
    created_at: str = field(default_factory=lambda: _timestamp())
    updated_at: str = field(default_factory=lambda: _timestamp())

    @classmethod
    def create(
        cls,
        *,
        product_kind: str,
        plan_id: str,
        idempotency_key: IdempotencyKey,
        metadata: Optional[Mapping[str, Any]] = None,
        run_id: Optional[str] = None,
    ) -> "ProductRun":
        if not product_kind:
            raise ValueError("ProductRun.product_kind is required")
        if not idempotency_key:
            raise ValueError("ProductRun.idempotency_key is required")
        return cls(
            run_id=run_id or f"run-{uuid.uuid4()}",
            product_kind=product_kind,
            plan_id=plan_id,
            idempotency_key=idempotency_key,
            metadata=dict(metadata or {}),
        )

    @property
    def terminal(self) -> bool:
        return self.observed_status in {
            PlanStatus.SUCCEEDED,
            PlanStatus.FAILED,
            PlanStatus.BLOCKED,
            PlanStatus.CANCELLED,
        }

    @property
    def latest_attempt(self) -> Optional[Attempt]:
        return self.attempts[-1] if self.attempts else None

    def attempts_for_step(self, step_id: str) -> Tuple[Attempt, ...]:
        return tuple(item for item in self.attempts if item.step_id == step_id)

    def with_attempt(self, attempt: Attempt) -> "ProductRun":
        if self.terminal:
            raise ValueError(f"Terminal ProductRun {self.run_id} is immutable")
        if attempt.run_id != self.run_id:
            raise ValueError("Attempt run identity must match ProductRun")
        if any(item.attempt_id == attempt.attempt_id for item in self.attempts):
            raise ValueError(f"Duplicate Attempt identity: {attempt.attempt_id}")
        return replace(self, attempts=self.attempts + (attempt,))

    def with_observed_attempt(self, observed: Attempt) -> "ProductRun":
        previous = next((item for item in self.attempts if item.attempt_id == observed.attempt_id), None)
        if previous is None:
            raise KeyError(f"Unknown Attempt: {observed.attempt_id}")
        if previous.status.terminal and observed != previous:
            raise ValueError(f"Terminal Attempt {observed.attempt_id} is immutable")
        attempts = tuple(observed if item.attempt_id == observed.attempt_id else item for item in self.attempts)
        step_statuses = dict(self.step_statuses)
        step_statuses[observed.step_id] = _step_status_from_attempt(observed.status)
        return replace(self, attempts=attempts, step_statuses=step_statuses)

    def with_step_status(self, step_id: str, status: StepStatus) -> "ProductRun":
        values = dict(self.step_statuses)
        values[step_id] = status
        return replace(self, step_statuses=values)

    def with_desired_state(self, desired_state: DesiredState) -> "ProductRun":
        if self.terminal and desired_state is not self.desired_state:
            raise ValueError(f"Terminal ProductRun {self.run_id} is immutable")
        return replace(self, desired_state=desired_state)

    def with_observed_status(self, observed_status: PlanStatus) -> "ProductRun":
        if self.terminal and observed_status is not self.observed_status:
            raise ValueError(f"Terminal ProductRun {self.run_id} is immutable")
        return replace(self, observed_status=observed_status)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "run_id": self.run_id,
            "product_kind": self.product_kind,
            "plan_id": self.plan_id,
            "idempotency_key": str(self.idempotency_key),
            "desired_state": self.desired_state.value,
            "observed_status": self.observed_status.value,
            "generation": int(self.generation),
            "attempts": [item.to_dict() for item in self.attempts],
            "step_statuses": {key: value.value for key, value in self.step_statuses.items()},
            "metadata": _normalized(self.metadata),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "ProductRun":
        return cls(
            run_id=str(data["run_id"]),
            product_kind=str(data["product_kind"]),
            plan_id=str(data["plan_id"]),
            idempotency_key=IdempotencyKey(str(data["idempotency_key"])),
            desired_state=DesiredState(str(data.get("desired_state", DesiredState.ACTIVE.value))),
            observed_status=PlanStatus(str(data.get("observed_status", PlanStatus.PENDING.value))),
            generation=Generation(int(data.get("generation", 0))),
            attempts=tuple(Attempt.from_dict(item) for item in data.get("attempts", ())),
            step_statuses={key: StepStatus(value) for key, value in data.get("step_statuses", {}).items()},
            metadata=dict(data.get("metadata", {})),
            created_at=str(data.get("created_at", _timestamp())),
            updated_at=str(data.get("updated_at", _timestamp())),
        )


def _artifact_dict(item: ArtifactRef) -> Dict[str, Any]:
    return _normalized(item.to_dict())


def _normalized(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _normalized(value[key]) for key in sorted(value, key=lambda item: str(item))}
    if isinstance(value, (tuple, list)):
        return [_normalized(item) for item in value]
    if hasattr(value, "to_dict"):
        return _normalized(value.to_dict())
    return value


def _optional_string(value: Any) -> Optional[str]:
    return None if value is None else str(value)


def _timestamp() -> str:
    return datetime.now(timezone.utc).isoformat()


def _step_status_from_attempt(status: AttemptStatus) -> StepStatus:
    return {
        AttemptStatus.PENDING: StepStatus.PENDING,
        AttemptStatus.RUNNING: StepStatus.RUNNING,
        AttemptStatus.SUCCEEDED: StepStatus.SUCCEEDED,
        AttemptStatus.FAILED: StepStatus.FAILED,
        AttemptStatus.LOST: StepStatus.FAILED,
        AttemptStatus.BLOCKED: StepStatus.BLOCKED,
        AttemptStatus.CANCELLING: StepStatus.CANCELLING,
        AttemptStatus.CANCELLED: StepStatus.CANCELLED,
    }[status]


__all__ = [
    "CONTRACT_VERSION", "Attempt", "AttemptId", "AttemptNumber", "AttemptStatus", "DesiredState",
    "ExecutionPlan", "Generation", "IdempotencyKey", "PlanStatus", "PlanStep", "ProductRun",
    "RetryPolicy", "StepDependency", "StepStatus",
]
