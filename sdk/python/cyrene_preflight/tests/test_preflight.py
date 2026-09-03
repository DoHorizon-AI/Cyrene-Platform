# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/tests/test_preflight.py
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

from cyrene_preflight import (
    AcceleratorFacts, CompatibilityRequest, EnvironmentCompatibility, HardwareFacts,
    ModelAnalysisRequest, PreflightStatus, ReferenceCompatibilityEvaluator, ReferenceModelAnalyzer,
)


GiB = 1024**3


def hardware(*, memory_gib: int = 24, runtime: str = "cuda-12", driver: str = "550.54") -> HardwareFacts:
    return HardwareFacts.from_node_resource_inventory(
        node_id="node-a", inventory_generation=7, accelerator_runtime=runtime, driver_version=driver,
        accelerators=(AcceleratorFacts(
            device_id="gpu-0", kind="gpu", vendor="nvidia", total_memory_bytes=memory_gib * GiB,
            allocatable_memory_bytes=memory_gib * GiB,
            features=("precision:fp16", "precision:bf16", "precision:fp8"),
        ),),
    )


def request(*, node: HardwareFacts | None, model_id: str = "example-1b", runtime: str = "cuda-12") -> CompatibilityRequest:
    model = ReferenceModelAnalyzer().analyze(ModelAnalysisRequest(
        model_id=model_id, precision="bf16", execution_kind="train",
        accelerator_memory_bytes=None if node is None else node.largest_accelerator_memory_bytes,
    ))
    return CompatibilityRequest(
        model=model,
        environment=EnvironmentCompatibility("sha256:environment-lock", runtime, "525.0"),
        hardware=node,
        execution_kind="train",
    )


def test_same_inputs_produce_deterministic_result():
    evaluator = ReferenceCompatibilityEvaluator()
    assert evaluator.evaluate(request(node=hardware())) == evaluator.evaluate(request(node=hardware()))
    assert evaluator.evaluate(request(node=hardware())).to_result(
        environment_identity="lock", hardware=hardware()
    ).status is PreflightStatus.READY


def test_insufficient_vram_is_blocked_with_evidence_and_remediation():
    node = hardware(memory_gib=1)
    result = ReferenceCompatibilityEvaluator().evaluate(request(node=node, model_id="example-7b")).to_result(
        environment_identity="lock", hardware=node
    )
    issue = next(item for item in result.issues if item.code == "accelerator.memory.insufficient")
    assert result.status is PreflightStatus.BLOCKED
    assert issue.evidence and issue.remediation


def test_runtime_mismatch_is_blocked():
    node = hardware(runtime="cuda-11")
    result = ReferenceCompatibilityEvaluator().evaluate(request(node=node, runtime="cuda-12")).to_result(
        environment_identity="lock", hardware=node
    )
    assert result.status is PreflightStatus.BLOCKED
    assert any(item.code == "runtime.accelerator.mismatch" for item in result.issues)


def test_missing_hardware_evidence_is_unknown():
    result = ReferenceCompatibilityEvaluator().evaluate(request(node=None)).to_result(
        environment_identity="lock", hardware=None
    )
    assert result.status is PreflightStatus.UNKNOWN
    assert any(item.code == "hardware.inventory.missing" for item in result.issues)


def test_noncanonical_hardware_source_is_rejected():
    with pytest.raises(ValueError, match="Node resource inventory"):
        HardwareFacts(node_id="node-a", inventory_generation=1, source_ref="external-probe", source="nvidia-probe")


def test_platform_preflight_has_no_product_imports():
    source_root = Path(__file__).resolve().parents[1] / "src" / "cyrene_preflight"
    forbidden = {"cy_exec", "training", "cyrene_yield"}
    for source in source_root.glob("*.py"):
        tree = ast.parse(source.read_text(encoding="utf-8"))
        imported = []
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.extend(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.append(node.module)
        assert not any(item.split(".", 1)[0] in forbidden for item in imported)
