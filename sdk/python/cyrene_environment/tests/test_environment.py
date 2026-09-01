# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_environment/tests/test_environment.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import ast
from pathlib import Path

import pytest

from cy_environment import (
    EnvironmentCandidate,
    EnvironmentLock,
    EnvironmentResolutionStatus,
    EnvironmentResolver,
    EnvironmentSpec,
    HardwareRuntimeFacts,
)


def _candidate(candidate_id: str, **overrides) -> EnvironmentCandidate:
    values = {
        "candidate_id": candidate_id,
        "runtime_profile": "local",
        "python_version": "3.12.4",
        "engine": "native_transformers",
        "framework_versions": {"torch": "2.4.0"},
        "resolution_provenance": "test-catalog",
    }
    values.update(overrides)
    return EnvironmentCandidate(**values)


def test_environment_lock_digest_is_deterministic_and_excludes_metadata():
    first = _candidate("first").to_lock().with_digest(created_at="2026-01-01T00:00:00+00:00")
    second = _candidate("second").to_lock().with_digest(created_at="2026-02-01T00:00:00+00:00")

    assert first.environment_digest == second.environment_digest
    assert first.to_dict()["created_at"] != second.to_dict()["created_at"]
    assert first.computed_digest() == first.environment_digest


def test_unresolved_lock_cannot_be_serialized_as_a_reproducible_lock():
    unresolved = EnvironmentLock(
        schema_version="1",
        runtime_profile="local",
        python_version="3.12.4",
    )

    assert unresolved.is_resolved is False
    with pytest.raises(ValueError, match="environment_digest"):
        unresolved.to_dict()


def test_lock_rejects_tampered_digest_and_spec_rejects_mutable_reference():
    lock = _candidate("lock").to_lock().with_digest(created_at="2026-01-01T00:00:00+00:00")
    tampered = dict(lock.to_dict())
    tampered["python_version"] = "3.13.0"

    with pytest.raises(ValueError, match="does not match"):
        EnvironmentLock.from_dict(tampered)
    with pytest.raises(TypeError, match="ArtifactRef"):
        EnvironmentSpec(base_image_ref="python:latest")


def test_resolver_selects_the_same_candidate_deterministically():
    spec = EnvironmentSpec(
        runtime_profile="local",
        python_version_constraint=">=3.12,<3.13",
        engine="native_transformers",
        framework_requirements={"torch": "==2.4"},
    )
    facts = HardwareRuntimeFacts(architecture="x86_64", precision="bf16")
    candidates = [
        _candidate("zeta", supported_architectures=("x86_64",), supported_precisions=("bf16",)),
        _candidate("alpha", supported_architectures=("x86_64",), supported_precisions=("bf16",)),
    ]

    result = EnvironmentResolver().resolve(spec, list(reversed(candidates)), facts)

    assert result.status is EnvironmentResolutionStatus.COMPATIBLE
    assert result.lock is not None
    assert result.lock.environment_digest == _candidate("alpha").to_lock().computed_digest()


def test_resolver_blocks_incompatible_python_framework_and_runtime():
    spec = EnvironmentSpec(
        runtime_profile="local",
        python_version_constraint="==3.12",
        engine="native_transformers",
        framework_requirements={"torch": "==2.4.0"},
        accelerator_runtime="cuda-12",
    )
    candidate = _candidate(
        "blocked",
        python_version="3.11.9",
        framework_versions={"torch": "2.3.0"},
        accelerator_runtime="cuda-11",
    )

    result = EnvironmentResolver().resolve(spec, [candidate])

    assert result.status is EnvironmentResolutionStatus.BLOCKED
    assert result.lock is None
    assert len(result.reasons) >= 3


def test_resolver_keeps_missing_evidence_unknown():
    spec = EnvironmentSpec(runtime_profile="local", engine="native_transformers")
    unknown_candidate = _candidate("unknown", engine=None)

    result = EnvironmentResolver().resolve(spec, [unknown_candidate])
    empty_catalog = EnvironmentResolver().resolve(spec, [])

    assert result.status is EnvironmentResolutionStatus.UNKNOWN
    assert empty_catalog.status is EnvironmentResolutionStatus.UNKNOWN
    assert result.lock is None


def test_contract_dependency_direction_is_one_way():
    package_root = Path(__file__).resolve().parents[1] / "src" / "cy_environment"
    artifact_root = Path(__file__).resolve().parents[1].parent / "cy-artifacts" / "src" / "cy_artifacts"

    for source in package_root.glob("*.py"):
        tree = ast.parse(source.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                assert all(not alias.name.startswith("cy_exec.training") for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                assert not node.module.startswith("cy_exec.training")

    for source in artifact_root.glob("*.py"):
        tree = ast.parse(source.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                assert all(not alias.name.startswith("cy_environment") for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                assert not node.module.startswith("cy_environment")
