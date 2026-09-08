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

import cyrene_preflight
from cyrene_preflight import (
    AcceleratorFacts,
    HardwareFacts,
)


GiB = 1024**3


def hardware(*, memory_gib: int = 24, runtime: str = "cuda-12", driver: str = "550.54") -> HardwareFacts:
    return HardwareFacts.from_node_resource_inventory(
        node_id="node-a",
        inventory_generation=7,
        accelerator_runtime=runtime,
        driver_version=driver,
        accelerators=(
            AcceleratorFacts(
                device_id="gpu-0",
                kind="gpu",
                vendor="nvidia",
                total_memory_bytes=memory_gib * GiB,
                allocatable_memory_bytes=memory_gib * GiB,
                features=("precision:fp16", "precision:bf16", "precision:fp8"),
            ),
        ),
    )


def test_hardware_facts_are_a_canonical_inventory_projection():
    facts = hardware()
    assert facts.source == "cyrene.core.v1.node-resource-inventory"
    assert facts.inventory_generation == 7
    assert facts.available_accelerator_memory_bytes == 24 * GiB
    assert facts.precision_support("bf16") is True
    assert facts.precision_support("int4") is False


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


def test_platform_preflight_does_not_export_product_model_contracts():
    for name in (
        "CompatibilityEvaluator",
        "CompatibilityRequest",
        "ModelAnalysisRequest",
        "ModelAnalyzer",
        "ModelFacts",
        "VramEstimate",
    ):
        assert not hasattr(cyrene_preflight, name)
