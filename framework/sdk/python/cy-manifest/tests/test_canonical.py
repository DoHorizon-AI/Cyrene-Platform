"""Determinism + cross-language known-answer tests for cy_manifest.

The KNOWN_RUNTIME_ID here MUST equal the value asserted by the Rust suite in
``contracts/rust/cy-manifest/src/lib.rs``. That shared literal is the cross-language
guarantee of M1: Rust and Python canonicalize byte-for-byte identically.
"""

from __future__ import annotations

import json
from pathlib import Path

from cy_manifest import (
    ArtifactManifest,
    RuntimeManifest,
    TrainingRevision,
    artifact_id,
    revision_id,
    runtime_id,
)

# Same literal asserted in contracts/rust/cy-manifest/src/lib.rs (KNOWN_RUNTIME_ID).
KNOWN_RUNTIME_ID = (
    "sha256:91fe1b35cf0dfd7f5162ef0895cd288e094111add6e8ec136fdda445d11b5445"
)

_REPO_ROOT = Path(__file__).resolve().parents[5]
_EXAMPLE_JSON = _REPO_ROOT / "contracts" / "schemas" / "examples" / "runtime_manifest.example.json"
_ARTIFACT_JSON = _REPO_ROOT / "contracts" / "schemas" / "examples" / "artifact_manifest.example.json"
_REVISION_JSON = _REPO_ROOT / "contracts" / "schemas" / "examples" / "training_revision.example.json"

# Same literals asserted in contracts/rust/cy-manifest/src/lib.rs.
KNOWN_ARTIFACT_ID = (
    "sha256:25c57c49626124d95c0a6135e71a3c5d7ec78a22736aa884f988ede8ea43c77c"
)
KNOWN_REVISION_ID = (
    "sha256:21fdd1007947fc2ab7d8ecea3684d598de3f7ed51bf63df0ee75207fe80a035a"
)


def _sample() -> RuntimeManifest:
    return RuntimeManifest(
        runtime_id="sha256:" + "0" * 64,
        workload="finetune",
        hardware_profile={
            "gpu_model": "NVIDIA A100-SXM4-40GB",
            "gpu_count": 1,
            "vram_gb": 40,
            "driver_version": "550.90.07",
            "cuda_max_supported": "12.4",
        },
        python="3.12.13",
        cuda_runtime="12.4.1",
        torch="2.4.0",
        frameworks={"transformers": "4.44.2", "peft": "0.12.0", "trl": "0.9.6"},
        precision="bf16",
        training_strategy="qlora",
        base_image_digest="sha256:" + "a" * 64,
        uv_lock_digest="sha256:" + "b" * 64,
        validation_level="resolved",
    )


def test_canonical_bytes_are_deterministic() -> None:
    m = _sample()
    assert m.canonical_bytes() == m.canonical_bytes()
    assert runtime_id(m) == runtime_id(m)


def test_runtime_id_excludes_runtime_id_field() -> None:
    a = _sample()
    b = _sample()
    a.runtime_id = None
    b.runtime_id = "sha256:" + "f" * 64
    assert runtime_id(a) == runtime_id(b)


def test_frameworks_key_order_does_not_matter() -> None:
    a = _sample()
    b = _sample()
    b.frameworks = {"trl": "0.9.6", "peft": "0.12.0", "transformers": "4.44.2"}
    assert runtime_id(a) == runtime_id(b)


def test_known_answer_matches() -> None:
    assert runtime_id(_sample()) == KNOWN_RUNTIME_ID


def test_example_file_matches_sample() -> None:
    data = json.loads(_EXAMPLE_JSON.read_text(encoding="utf-8"))
    parsed = RuntimeManifest.model_validate(data)
    assert runtime_id(parsed) == KNOWN_RUNTIME_ID


# -- Immutable-resource known-answer tests (mirror the Rust suite) ----------


def _artifact() -> ArtifactManifest:
    data = json.loads(_ARTIFACT_JSON.read_text(encoding="utf-8"))
    return ArtifactManifest.model_validate(data)


def _revision() -> TrainingRevision:
    data = json.loads(_REVISION_JSON.read_text(encoding="utf-8"))
    return TrainingRevision.model_validate(data)


def test_artifact_known_answer_matches() -> None:
    assert artifact_id(_artifact()) == KNOWN_ARTIFACT_ID


def test_revision_known_answer_matches() -> None:
    assert revision_id(_revision()) == KNOWN_REVISION_ID


def test_artifact_id_excludes_artifact_id_field() -> None:
    a = _artifact()
    b = _artifact()
    a.artifact_id = None
    b.artifact_id = "sha256:" + "f" * 64
    assert artifact_id(a) == artifact_id(b)


def test_revision_id_excludes_revision_id_field() -> None:
    a = _revision()
    b = _revision()
    a.revision_id = None
    b.revision_id = "sha256:" + "f" * 64
    assert revision_id(a) == revision_id(b)


def test_immutable_ids_are_deterministic() -> None:
    assert artifact_id(_artifact()) == artifact_id(_artifact())
    assert revision_id(_revision()) == revision_id(_revision())


# -- RFC 8785 float-battery known-answer (mirrors the Rust suite) -----------
#
# A revision whose free-form snapshot contains fractional/exponential floats.
# Under the old hand-rolled canonicalizer this hashed DIFFERENTLY in Python
# (repr -> "2e-05") vs Rust (Display -> "0.00002"); under RFC 8785 (JCS) both
# sides now agree on this literal.
KNOWN_ADVERSARIAL_REVISION_ID = (
    "sha256:1e71cfa0560a8a2ca65ff04e09585c4ccd4c9dd549c4682b3531e18e8f5cafd8"
)


def _adversarial_revision() -> TrainingRevision:
    return TrainingRevision.model_validate(
        {
            "revision_id": "sha256:" + "0" * 64,
            "run_id": "run-adv",
            "reason": "float_battery",
            "input_metrics": {
                "lr": 0.00002,
                "loss": 0.42137624,
                "grad_norm": 1.7320508075688772,
                "big": 1000000.0,
            },
            "decision_rule": "test.v1",
            "config_before": {},
            "config_after": {},
            "state_retention": {
                "model": True,
                "optimizer": True,
                "scheduler": True,
                "grad_scaler": False,
            },
            "rolled_back": False,
            "actor": "rule",
        }
    )


def test_adversarial_float_known_answer_matches() -> None:
    assert revision_id(_adversarial_revision()) == KNOWN_ADVERSARIAL_REVISION_ID
