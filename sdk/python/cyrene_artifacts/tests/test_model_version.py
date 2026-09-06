# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/tests/test_model_version.py     ║
# ║ Module: CYRENE Platform                                             ║
# ║ Role: Immutable composed ModelVersion contract tests.                ║
# ║                                                                     ║
# ║ 模块：CYRENE Platform                                                ║
# ║ 职责：不可变组合 ModelVersion 契约测试。                               ║
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import copy
from typing import MutableMapping, cast

import pytest

from cy_artifacts import ModelVersion


def _artifact(digest_character: str, *, size_bytes: int = 10, kind: str = "model") -> dict:
    digest = "sha256:" + digest_character * 64
    artifact = {
        "uri": "artifact://sha256/" + digest_character * 64,
        "digest": digest,
        "size_bytes": size_bytes,
        "kind": kind,
    }
    if kind == "model":
        artifact["manifest_digest"] = digest
    return artifact


def _composed_payload() -> dict:
    return {
        "schemaVersion": "1",
        "composition": "BASE_PLUS_LORA",
        "baseModel": {
            "artifact": _artifact("a", size_bytes=100),
            "source": {
                "repository": "Qwen/Qwen2.5-1.5B-Instruct",
                "revision": "1" * 40,
            },
        },
        "adapterArtifact": _artifact("b", size_bytes=20),
        "tokenizer": {"mode": "INHERIT"},
        "chatTemplate": {"mode": "OVERRIDE", "artifact": _artifact("c", kind="generic")},
        "lineage": {
            "trainingRun": "training-run://yield/run-123",
            "datasetVersion": "dataset-version://yield/dataset-456",
            "inputArtifacts": [_artifact("d", kind="dataset")],
        },
    }


def test_composed_model_version_has_content_identity_and_detached_immutable_state() -> None:
    model_version = ModelVersion.create(_composed_payload())
    payload = model_version.to_dict()

    assert model_version.composition == "BASE_PLUS_LORA"
    assert model_version.id.startswith("model-version://sha256/")
    assert payload["id"] == model_version.id
    assert "rank" not in payload
    assert "alpha" not in payload
    assert "target_modules" not in payload

    payload["lineage"]["inputArtifacts"].clear()
    assert len(model_version.to_dict()["lineage"]["inputArtifacts"]) == 1
    assert ModelVersion.from_dict(model_version.to_dict()).id == model_version.id


def test_model_version_id_is_order_independent_and_tamper_evident() -> None:
    payload = _composed_payload()
    reordered = {key: payload[key] for key in reversed(list(payload))}

    first = ModelVersion.create(payload)
    second = ModelVersion.create(reordered)
    assert first.id == second.id
    assert first.canonical_bytes() == second.canonical_bytes()

    tampered = first.to_dict()
    tampered["lineage"]["trainingRun"] = "training-run://yield/other"
    with pytest.raises(ValueError, match="does not match"):
        ModelVersion.from_dict(tampered)


def test_model_version_has_no_raw_constructor_and_rejects_invalid_ids() -> None:
    with pytest.raises(TypeError, match=r"create\(\) or from_dict\(\)"):
        ModelVersion(_composed_payload(), "model-version://sha256/" + "0" * 64)

    model_version = ModelVersion.create(_composed_payload())
    with pytest.raises(TypeError):
        cast(MutableMapping[str, object], model_version._payload)["composition"] = "FULL_MODEL"

    tampered = model_version.to_dict()
    tampered["id"] = "not-a-model-version-uri"
    with pytest.raises(ValueError, match="model-version://sha256"):
        ModelVersion.from_dict(tampered)


def test_full_model_version_has_no_composed_fields() -> None:
    payload = _composed_payload()
    payload.pop("baseModel")
    payload.pop("adapterArtifact")
    payload["composition"] = "FULL_MODEL"
    payload["fullModelArtifact"] = _artifact("e", size_bytes=1000)

    model_version = ModelVersion.create(payload)
    assert model_version.composition == "FULL_MODEL"
    assert "fullModelArtifact" in model_version.to_dict()
    assert "baseModel" not in model_version.to_dict()
    assert "adapterArtifact" not in model_version.to_dict()


@pytest.mark.parametrize(
    ("change", "message"),
    [
        (lambda payload: payload.update({"unknown": True}), "unknown fields"),
        (lambda payload: payload["baseModel"].update({"adapter": []}), "unknown fields"),
        (lambda payload: payload["adapterArtifact"].pop("manifest_digest"), "portable directory"),
        (
            lambda payload: payload["adapterArtifact"].update({"manifest_digest": None}),
            "non-empty string",
        ),
        (lambda payload: payload["adapterArtifact"].update({"kind": "generic"}), "kind 'model'"),
        (lambda payload: payload["tokenizer"].update({"artifact": _artifact("f")}), "forbidden"),
        (
            lambda payload: payload["chatTemplate"].pop("artifact"),
            "required",
        ),
        (lambda payload: payload["baseModel"]["source"].update({"revision": "not-a-revision"}), "40-hex"),
    ],
)
def test_model_version_rejects_invalid_composition_shapes(change, message) -> None:
    payload = _composed_payload()
    change(payload)
    with pytest.raises(ValueError, match=message):
        ModelVersion.create(payload)


def test_model_version_rejects_floats_and_product_state() -> None:
    payload = _composed_payload()
    payload["baseModel"]["artifact"]["size_bytes"] = 1.0
    with pytest.raises(ValueError, match="floating-point"):
        ModelVersion.create(payload)

    payload = _composed_payload()
    payload["lineage"]["state"] = "COMPLETED"
    with pytest.raises(ValueError, match="unknown fields"):
        ModelVersion.create(payload)


def test_model_version_lineage_and_override_artifacts_are_strict() -> None:
    payload = _composed_payload()
    payload["lineage"]["trainingRun"] = "mutable-id-without-uri"
    with pytest.raises(ValueError, match="opaque URI"):
        ModelVersion.create(payload)

    payload = _composed_payload()
    payload["chatTemplate"]["artifact"]["name"] = "display-only"
    with pytest.raises(ValueError, match="unknown fields"):
        ModelVersion.create(payload)

    payload = _composed_payload()
    payload["adapterArtifact"]["digest"] = "sha256:" + "c" * 64
    with pytest.raises(ValueError, match="match its digest"):
        ModelVersion.create(payload)


def test_model_version_create_requires_id_omitted_and_from_dict_requires_id() -> None:
    payload = _composed_payload()
    with pytest.raises(ValueError, match="expects the id"):
        ModelVersion.create({**payload, "id": "model-version://sha256/" + "a" * 64})

    with pytest.raises(ValueError, match="missing fields"):
        ModelVersion.from_dict(payload)


def test_model_version_input_payload_is_not_mutated() -> None:
    payload = _composed_payload()
    original = copy.deepcopy(payload)
    ModelVersion.create(payload)
    assert payload == original


def test_future_merge_creates_a_new_full_identity_and_preserves_composed_source() -> None:
    composed = ModelVersion.create(_composed_payload())
    original_composed = composed.to_dict()

    merged_payload = _composed_payload()
    merged_payload.pop("baseModel")
    merged_payload.pop("adapterArtifact")
    merged_payload["composition"] = "FULL_MODEL"
    merged_payload["fullModelArtifact"] = _artifact("e", size_bytes=1000)
    merged_payload["lineage"] = {"derivedFromModelVersion": composed.id}

    merged = ModelVersion.create(merged_payload)

    assert merged.composition == "FULL_MODEL"
    assert merged.id != composed.id
    assert merged.to_dict()["lineage"]["derivedFromModelVersion"] == composed.id
    assert composed.to_dict() == original_composed
    assert ModelVersion.from_dict(original_composed).id == composed.id
