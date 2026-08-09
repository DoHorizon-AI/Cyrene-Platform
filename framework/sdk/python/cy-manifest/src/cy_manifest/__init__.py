"""Standalone shared package for CYRENE manifest models and canonical hashing.

This package is intentionally decoupled from ``cy_exec`` and
``cy_control_plane`` so it can be imported by any layer (a future Python control
plane, the execution layer, tooling, etc.). It mirrors the Rust crate
``cy-manifest`` and produces byte-identical canonical hashes.
"""

from __future__ import annotations

from .canonical import canonical_sha256_hex, canonicalize
from .models import (
    ArtifactManifest,
    Check,
    CheckpointMetadata,
    Constraints,
    CpuInfo,
    Decision,
    GpuInfo,
    HardwareManifest,
    HardwareProfile,
    Interconnect,
    Lineage,
    ModelManifest,
    Objective,
    OsInfo,
    PrecisionSupport,
    RuntimeManifest,
    StateRetention,
    TrainingRevision,
    ValidationResult,
    VramEstimate,
    WhyReport,
    WorkloadRequest,
    artifact_id,
    checkpoint_id,
    revision_id,
    runtime_id,
)

__all__ = [
    "canonicalize",
    "canonical_sha256_hex",
    "runtime_id",
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

__version__ = "0.1.0"
