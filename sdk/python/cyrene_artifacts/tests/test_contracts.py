# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/tests/test_contracts.py
# ║ Module: CYRENE Platform
# ║ Role: Provider-neutral Artifact identity tests.
# ║ 中文：验证与提供方无关的 Artifact 身份规则。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：验证与 Product 无关的 Artifact 身份契约。
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import ast
from pathlib import Path

import pytest

from cy_artifacts import ArtifactKind, ArtifactRef


def test_artifact_ref_round_trips_opaque_kind() -> None:
    digest = "sha256:" + "a" * 64
    ref = ArtifactRef(
        uri="",
        digest=digest,
        size_bytes=7,
        kind=ArtifactKind("product-owned-category"),
        manifest_digest=digest,
    )

    assert ArtifactRef.from_dict(ref.to_dict()) == ref
    assert ref.to_dict()["kind"] == "product-owned-category"
    assert "name" not in ref.to_dict()


def test_artifact_kind_rejects_invalid_identifiers() -> None:
    for value in ["", "   ", "bad\nkind", "x" * 129]:
        with pytest.raises(ValueError):
            ArtifactKind(value)


def test_artifact_package_has_no_product_imports() -> None:
    source_root = Path(__file__).parents[1] / "src" / "cy_artifacts"
    forbidden = ("cy_exec.training", "cy_manifest.models")
    for source in source_root.rglob("*.py"):
        tree = ast.parse(source.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                assert all(not alias.name.startswith(forbidden) for alias in node.names)
            if isinstance(node, ast.ImportFrom) and node.module:
                assert not node.module.startswith(forbidden)
