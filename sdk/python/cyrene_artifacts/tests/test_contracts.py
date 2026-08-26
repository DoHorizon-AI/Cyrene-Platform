from __future__ import annotations

import ast
import json
from pathlib import Path

from cy_artifacts import ArtifactKind, ArtifactLineage, ArtifactManifest, ArtifactRef


FIXTURE = Path(__file__).parent / "fixtures" / "artifact_manifest.example.json"


def test_platform_artifact_manifest_fixture_round_trips_and_matches_golden_id():
    payload = json.loads(FIXTURE.read_text(encoding="utf-8"))
    manifest = ArtifactManifest.from_dict(payload)

    assert manifest.to_dict() == payload
    assert manifest.computed_artifact_id() == (
        "sha256:25c57c49626124d95c0a6135e71a3c5d7ec78a22736aa884f988ede8ea43c77c"
    )


def test_artifact_ref_round_trips_canonical_wire_fields():
    digest = "sha256:" + "a" * 64
    ref = ArtifactRef(
        uri="",
        digest=digest,
        size_bytes=7,
        kind=ArtifactKind.MODEL,
        manifest_digest=digest,
    )

    assert ArtifactRef.from_dict(ref.to_dict()) == ref
    assert "name" not in ref.to_dict()


def test_artifact_lineage_is_the_public_input_projection():
    lineage = ArtifactLineage(revision_chain=("sha256:revision",))
    manifest = ArtifactManifest(
        kind=ArtifactKind.MODEL,
        integrity="sha256:" + "b" * 64,
        lineage=lineage,
    )

    assert manifest.to_dict()["lineage"] == {"revision_chain": ["sha256:revision"]}


def test_artifact_package_has_no_training_imports():
    source_root = Path(__file__).parents[1] / "src" / "cy_artifacts"
    for source in source_root.rglob("*.py"):
        tree = ast.parse(source.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                assert all(not alias.name.startswith("cy_exec.training") for alias in node.names)
            if isinstance(node, ast.ImportFrom) and node.module:
                assert not node.module.startswith("cy_exec.training")
