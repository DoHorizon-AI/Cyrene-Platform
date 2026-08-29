"""Pydantic v2 models for the core CYRENE manifests.

Mirrors the JSON Schemas under ``schemas/manifests/`` (source of truth) and the
Rust types in ``contracts/rust/cy-manifest``. The canonicalization/hash behaviour is
kept identical to the Rust side; see ``schemas/CANONICALIZATION.md``.
"""

from __future__ import annotations

from typing import Any, Literal, Optional

from pydantic import BaseModel, ConfigDict, Field

from .canonical import canonical_sha256_hex, canonicalize

__all__ = [
    "OsInfo",
    "CpuInfo",
    "GpuInfo",
    "Interconnect",
    "PrecisionSupport",
    "HardwareManifest",
    "VramEstimate",
    "ModelManifest",
    "Objective",
    "Constraints",
    "WorkloadRequest",
    "HardwareProfile",
    "RuntimeManifest",
    "runtime_id",
    "Decision",
    "WhyReport",
    "Check",
    "ValidationResult",
    "StateRetention",
    "TrainingRevision",
    "revision_id",
    "CheckpointMetadata",
    "checkpoint_id",
    "Lineage",
    "ArtifactManifest",
    "artifact_id",
]

WeightPrecision = Literal["fp32", "fp16", "bf16", "fp8", "int8", "int4"]
ModelFormat = Literal["safetensors", "pytorch", "gguf", "awq", "gptq", "onnx"]
QuantizationScheme = Literal["none", "awq", "gptq", "bnb-nf4", "bnb-int8", "fp8"]
WorkloadKind = Literal["finetune", "serve"]
PriorityKind = Literal["latency", "throughput", "cost", "quality"]
TrainingStrategyKind = Literal["none", "full", "lora", "qlora", "deepspeed", "fsdp"]
ValidationLevelKind = Literal[
    "declared",
    "resolved",
    "built",
    "smoke-tested",
    "model-loaded",
    "validated",
    "benchmarked",
    "certified",
]
VerdictKind = Literal["chosen", "rejected"]
ConfidenceKind = Literal["low", "medium", "high"]
CheckStatusKind = Literal["pass", "fail", "skip"]
ActorKind = Literal["user", "rule", "ai"]
ArtifactKindKind = Literal["model", "dataset", "checkpoint", "merged", "quantized", "report"]


class _Manifest(BaseModel):
    """Base with the canonical-hashing helpers shared by every manifest."""

    model_config = ConfigDict(extra="forbid")

    def canonical_value(self) -> Any:
        """JSON value tree used as the hash preimage."""
        return self.model_dump(mode="json", exclude_none=True)

    def canonical_bytes(self) -> bytes:
        """Canonical byte serialization per ``schemas/CANONICALIZATION.md``."""
        return canonicalize(self.canonical_value())

    def canonical_sha256_hex(self) -> str:
        """Lowercase hex SHA-256 of :meth:`canonical_bytes`."""
        return canonical_sha256_hex(self.canonical_value())


# ---------------------------------------------------------------------------
# HardwareManifest
# ---------------------------------------------------------------------------


class OsInfo(_Manifest):
    name: str
    kernel: str
    glibc: str


class CpuInfo(_Manifest):
    arch: str
    cores: int
    threads: Optional[int] = None


class GpuInfo(_Manifest):
    model: str
    count: int
    vram_gb: float
    compute_capability: str


class Interconnect(_Manifest):
    nvlink: bool
    pcie_gen: Optional[int] = None


class PrecisionSupport(_Manifest):
    bf16: bool
    fp16: bool
    fp8: bool


class HardwareManifest(_Manifest):
    os: OsInfo
    cpu: CpuInfo
    memory_gb: float
    disk_gb: float
    gpus: list[GpuInfo]
    driver_version: str
    cuda_max_supported: str
    interconnect: Interconnect
    container_runtime: str
    precision_support: PrecisionSupport


# ---------------------------------------------------------------------------
# ModelManifest
# ---------------------------------------------------------------------------


class VramEstimate(_Manifest):
    train_gb: float
    infer_gb: float


class ModelManifest(_Manifest):
    architecture: str
    params: int
    weight_precision: WeightPrecision
    context_length: int
    format: ModelFormat
    remote_code: bool
    tokenizer: str
    chat_template: Optional[str] = None
    quantization: Optional[QuantizationScheme] = None
    vram_estimate: VramEstimate


# ---------------------------------------------------------------------------
# WorkloadRequest
# ---------------------------------------------------------------------------


class Objective(_Manifest):
    priority: PriorityKind


class Constraints(_Manifest):
    max_gpu_count: int
    max_cost: Optional[float] = None


class WorkloadRequest(_Manifest):
    model: str
    workload: WorkloadKind
    dataset: Optional[str] = None
    objective: Objective
    constraints: Constraints


# ---------------------------------------------------------------------------
# RuntimeManifest
# ---------------------------------------------------------------------------


class HardwareProfile(_Manifest):
    gpu_model: str
    gpu_count: int
    vram_gb: float
    driver_version: str
    cuda_max_supported: str


class RuntimeManifest(_Manifest):
    # Computed content id. EXCLUDED from the canonical hash preimage.
    runtime_id: Optional[str] = None
    workload: WorkloadKind
    hardware_profile: HardwareProfile
    python: str
    cuda_runtime: str
    torch: str
    frameworks: dict[str, str] = Field(default_factory=dict)
    precision: WeightPrecision
    training_strategy: TrainingStrategyKind
    base_image_digest: str
    uv_lock_digest: str
    validation_level: ValidationLevelKind

    def canonical_value(self) -> Any:
        value = self.model_dump(mode="json", exclude_none=True)
        value.pop("runtime_id", None)
        return value


def runtime_id(manifest: RuntimeManifest) -> str:
    """Compute the ``runtime_id`` of a :class:`RuntimeManifest`.

    ``runtime_id = "sha256:" + hex(sha256(canonical_bytes(manifest_without_runtime_id)))``.
    """
    return f"sha256:{manifest.canonical_sha256_hex()}"


# ---------------------------------------------------------------------------
# WhyReport
# ---------------------------------------------------------------------------


class Decision(_Manifest):
    subject: str
    verdict: VerdictKind
    rationale: str
    evidence: list[str]
    confidence: ConfidenceKind
    # Strength of the backing evidence on the project's evidence ladder.
    evidence_level: ValidationLevelKind


class WhyReport(_Manifest):
    decisions: list[Decision]
    summary: Optional[str] = None


# ---------------------------------------------------------------------------
# ValidationResult
# ---------------------------------------------------------------------------


class Check(_Manifest):
    name: str
    status: CheckStatusKind
    evidence: Optional[str] = None


class ValidationResult(_Manifest):
    # Reference to the validated RuntimeManifest ("sha256:<hex>").
    runtime_id: str
    level: ValidationLevelKind
    checks: list[Check]
    evidence_level: ValidationLevelKind


# ---------------------------------------------------------------------------
# TrainingRevision (immutable; content-hash id)
# ---------------------------------------------------------------------------


class StateRetention(_Manifest):
    model: bool
    optimizer: bool
    scheduler: bool
    grad_scaler: bool


class TrainingRevision(_Manifest):
    # Computed content id. EXCLUDED from the canonical hash preimage.
    revision_id: Optional[str] = None
    run_id: str
    parent_revision_id: Optional[str] = None
    reason: str
    input_metrics: dict[str, Any]
    decision_rule: str
    config_before: dict[str, Any]
    config_after: dict[str, Any]
    base_checkpoint_id: Optional[str] = None
    state_retention: StateRetention
    canary_result: Optional[dict[str, Any]] = None
    performance_delta: Optional[dict[str, Any]] = None
    rolled_back: bool
    actor: ActorKind

    def canonical_value(self) -> Any:
        value = self.model_dump(mode="json", exclude_none=True)
        value.pop("revision_id", None)
        return value


def revision_id(manifest: TrainingRevision) -> str:
    """Compute the ``revision_id`` of a :class:`TrainingRevision` (content hash, id excluded)."""
    return f"sha256:{manifest.canonical_sha256_hex()}"


# ---------------------------------------------------------------------------
# CheckpointMetadata (immutable; content-hash id)
# ---------------------------------------------------------------------------


class CheckpointMetadata(_Manifest):
    # Computed content id. EXCLUDED from the canonical hash preimage.
    checkpoint_id: Optional[str] = None
    run_id: str
    step: int
    epoch: Optional[int] = None
    path: Optional[str] = None
    digest: str
    metrics: dict[str, Any]
    revision_id: Optional[str] = None
    size_bytes: Optional[int] = None

    def canonical_value(self) -> Any:
        value = self.model_dump(mode="json", exclude_none=True)
        value.pop("checkpoint_id", None)
        return value


def checkpoint_id(manifest: CheckpointMetadata) -> str:
    """Compute the ``checkpoint_id`` of a :class:`CheckpointMetadata` (content hash, id excluded)."""
    return f"sha256:{manifest.canonical_sha256_hex()}"


# ---------------------------------------------------------------------------
# ArtifactManifest (immutable; content-hash id)
# ---------------------------------------------------------------------------


class Lineage(_Manifest):
    base_model_revision: Optional[str] = None
    dataset_digest: Optional[str] = None
    runtime_id: Optional[str] = None
    revision_chain: list[str]
    checkpoint_id: Optional[str] = None


class ArtifactManifest(_Manifest):
    # Computed content id. EXCLUDED from the canonical hash preimage.
    artifact_id: Optional[str] = None
    kind: ArtifactKindKind
    source: Optional[str] = None
    integrity: str
    size_bytes: Optional[int] = None
    lineage: Lineage
    ref_count: Optional[int] = None

    def canonical_value(self) -> Any:
        value = self.model_dump(mode="json", exclude_none=True)
        value.pop("artifact_id", None)
        return value


def artifact_id(manifest: ArtifactManifest) -> str:
    """Compute the ``artifact_id`` of an :class:`ArtifactManifest` (content hash, id excluded)."""
    return f"sha256:{manifest.canonical_sha256_hex()}"
