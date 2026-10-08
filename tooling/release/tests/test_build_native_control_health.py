"""
┌──────────────────────────────────────────────────────────────────────┐
│  📄 test_build_native_control_health.py                               │
│  Module: tooling.release.tests.test_build_native_control_health       │
│  Role: Verify signed native control-host readiness metadata.          │
│                                                                      │
│  模块职责：验证原生控制主机的签名 readiness 元数据。                    │
└──────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest

pytest.importorskip("rfc8785", reason="release-builder tests require tooling/release/requirements.txt")

from tooling.release import build_native_components as native
from tooling.release import component_artifacts as artifacts
from tooling.release.validate_component_schema import validate_document


C12_CATALOG_SHA256 = "4f0696f0f7dd24fe65d955a5269f3e2591517f4b75c79b7b7fced921dfb4bb81"
DEFAULT_CATALOG_PATH = Path("workspace-catalog/governance/component-catalog-v1.json")
CONTROL_COMPONENT_IDS = {"cy-workspace-authority-host", "cy-workspace-web-bff"}
TARGET_ID = "linux-ubuntu-24.04-x86_64-systemd"
SOURCE_COMMIT = "a" * 40


def _catalog_inputs() -> tuple[Path, str]:
    """Resolve the exact declared catalog and expected raw SHA-256.

    With no external declaration, retain the original C12 catalog baseline.
    An externally supplied path and digest must be provided together.
    """

    catalog_path = os.environ.get("CYRENE_COMPONENT_CATALOG")
    catalog_sha256 = os.environ.get("CYRENE_COMPONENT_CATALOG_SHA256")
    if catalog_path is None and catalog_sha256 is None:
        return DEFAULT_CATALOG_PATH, C12_CATALOG_SHA256
    if not catalog_path or not catalog_sha256:
        raise AssertionError(
            "external catalog input requires both CYRENE_COMPONENT_CATALOG and CYRENE_COMPONENT_CATALOG_SHA256"
        )
    if re.fullmatch(r"[0-9a-f]{64}", catalog_sha256) is None:
        raise AssertionError("external catalog digest must be 64 lowercase hexadecimal characters")
    return Path(catalog_path), catalog_sha256


def _read_declared_catalog(path: Path, expected_sha256: str) -> tuple[bytes, dict[str, Any]]:
    """Read catalog bytes only when they match the independently declared digest."""

    if re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is None:
        raise AssertionError("catalog digest must be 64 lowercase hexadecimal characters")
    raw = path.read_bytes()
    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != expected_sha256:
        raise AssertionError(f"catalog bytes at {path} do not match declared SHA-256 {expected_sha256}")
    catalog = json.loads(raw.decode("utf-8", "strict"))
    if not isinstance(catalog, dict):
        raise AssertionError("declared component catalog must be a JSON object")
    return raw, catalog


def _compatibility_from_catalog(catalog: dict[str, Any], group_id: str) -> dict[str, Any]:
    groups = catalog["compatibilityGroups"]
    group = next(row for row in groups if row["groupId"] == group_id)
    return {
        key: group[key] for key in ("groupId", "groupVersion", "contractApiVersion", "wireApiVersion", "contractLock")
    }


def test_native_control_manifests_bind_fixed_readiness_and_preserve_c11_contracts(tmp_path: Path) -> None:
    """Build from the declared verified catalog and preserve fixed C11 contracts."""
    catalog_path, catalog_sha256 = _catalog_inputs()
    _, catalog = _read_declared_catalog(catalog_path, catalog_sha256)

    repository = tmp_path / "source"
    repository.mkdir()
    for component_id in sorted(CONTROL_COMPONENT_IDS):
        unit = native.SYSTEMD_UNIT_COMPONENTS[component_id]
        unit_path = repository / "infrastructure/systemd" / unit
        unit_path.parent.mkdir(parents=True, exist_ok=True)
        unit_path.write_text("[Service]\n", encoding="utf-8")

    components = tuple(row for row in native.COMPONENTS if row["id"] in CONTROL_COMPONENT_IDS)
    package_metadata = {
        row["package"]: {
            "name": row["package"],
            "version": "0.1.0",
            "targets": [{"name": row["binary"], "kind": ["bin"]}],
        }
        for row in components
    }

    def run_native_command(command: list[str], *, cwd: Path) -> str:
        if command[:3] == ["git", "rev-parse", "HEAD"]:
            return SOURCE_COMMIT
        if command[:2] == ["cargo", "metadata"]:
            return json.dumps({"packages": list(package_metadata.values())})
        if command[:2] == ["cargo", "build"]:
            binary = command[command.index("--bin") + 1]
            binary_path = cwd / "target/x86_64-unknown-linux-gnu/release" / binary
            binary_path.parent.mkdir(parents=True, exist_ok=True)
            binary_path.write_bytes(f"built:{binary}".encode())
            binary_path.chmod(0o755)
            return ""
        raise AssertionError(f"unexpected native build command: {command}")

    output = tmp_path / "release"
    with (
        patch.object(native, "COMPONENTS", components),
        patch.object(native, "_run", side_effect=run_native_command),
        patch.object(native, "_assert_build_host_matches_target"),
        patch.object(native, "_validate_native_binary_abi"),
        patch.object(native, "_copy_attached_admin_binaries", return_value=[]),
        patch.object(native, "_verify_contract_lock"),
    ):
        manifest_paths = native.build(
            repository,
            output,
            channel="preview",
            source_ref="refs/heads/develop",
            source_commit=SOURCE_COMMIT,
            run_id="123456789",
            run_attempt=1,
            catalog_path=catalog_path,
            catalog_sha256=catalog_sha256,
            target_id=TARGET_ID,
        )

    assert len(manifest_paths) == len(CONTROL_COMPONENT_IDS)
    manifests = {json.loads(path.read_text(encoding="utf-8"))["componentId"]: path for path in manifest_paths}
    assert set(manifests) == CONTROL_COMPONENT_IDS
    expected_health = {
        "cy-workspace-authority-host": {"kind": "http", "port": 8080, "path": "/readyz"},
        "cy-workspace-web-bff": {"kind": "http", "port": 18084, "path": "/readyz"},
    }

    for component_id, manifest_path in manifests.items():
        row = next(item for item in catalog["components"] if item["componentId"] == component_id)
        document = artifacts.verify_manifest(
            manifest_path,
            output / "artifacts" / f"{component_id}-{TARGET_ID}.tar.gz",
            verify_run=False,
            verify_attestation=False,
        )
        assert document["health"] == expected_health[component_id]
        assert document["dependencies"] == row["dependencies"]
        assert document["restart"] == row["restart"]
        assert document["compatibility"] == _compatibility_from_catalog(catalog, row["compatibilityGroup"])
        assert document["source"] == {
            "repository": native.PLATFORM_REPOSITORY,
            "ref": "refs/heads/develop",
            "commit": SOURCE_COMMIT,
        }
        assert document["manifestDigest"] == artifacts._manifest_digest(document)
        assert validate_document(manifest_path, catalog_path.parent, "manifest") == []


def test_catalog_selection_keeps_c12_fallback_and_requires_a_complete_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Keep the pinned C12 default while rejecting partial external declarations."""

    monkeypatch.delenv("CYRENE_COMPONENT_CATALOG", raising=False)
    monkeypatch.delenv("CYRENE_COMPONENT_CATALOG_SHA256", raising=False)
    assert _catalog_inputs() == (DEFAULT_CATALOG_PATH, C12_CATALOG_SHA256)

    monkeypatch.setenv("CYRENE_COMPONENT_CATALOG", "/verified/catalog-v1.json")
    monkeypatch.setenv("CYRENE_COMPONENT_CATALOG_SHA256", "b" * 64)
    assert _catalog_inputs() == (Path("/verified/catalog-v1.json"), "b" * 64)

    monkeypatch.delenv("CYRENE_COMPONENT_CATALOG_SHA256", raising=False)
    with pytest.raises(AssertionError, match="requires both"):
        _catalog_inputs()

    monkeypatch.delenv("CYRENE_COMPONENT_CATALOG", raising=False)
    monkeypatch.setenv("CYRENE_COMPONENT_CATALOG_SHA256", "a" * 64)
    with pytest.raises(AssertionError, match="requires both"):
        _catalog_inputs()


def test_catalog_reader_rejects_bytes_that_do_not_match_declared_sha256(
    tmp_path: Path,
) -> None:
    """An independently declared catalog digest remains a raw-byte gate."""

    catalog_path = tmp_path / "component-catalog-v1.json"
    catalog_path.write_bytes(b'{"schemaVersion":1}\n')

    with pytest.raises(AssertionError, match="do not match declared SHA-256"):
        _read_declared_catalog(catalog_path, "0" * 64)


@pytest.mark.parametrize(
    "component_id",
    ["cy-workspace-authority-host", "cy-workspace-web-bff"],
)
def test_control_readiness_policy_is_exact_and_does_not_guess_other_ports(component_id: str) -> None:
    assert native.NATIVE_HTTP_READINESS[component_id] == {
        "kind": "http",
        "port": 8080 if component_id == "cy-workspace-authority-host" else 18084,
        "path": "/readyz",
    }
    assert set(native.NATIVE_HTTP_READINESS) == CONTROL_COMPONENT_IDS
