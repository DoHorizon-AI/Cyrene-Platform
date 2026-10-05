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
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest

pytest.importorskip("rfc8785", reason="release-builder tests require tooling/release/requirements.txt")

from tooling.release import build_native_components as native
from tooling.release import component_artifacts as artifacts
from tooling.release.validate_component_schema import validate_document


C11_CATALOG_SHA256 = "fc2d6dc485bfc6a6fbbef426c93acf2a640b771e6e24a7284b030804fd02fab0"
CATALOG_PATH = Path(
    os.environ.get("CYRENE_COMPONENT_CATALOG", "workspace-catalog/governance/component-catalog-v1.json")
)
CONTROL_COMPONENT_IDS = {"cy-workspace-authority-host", "cy-workspace-web-bff"}
TARGET_ID = "linux-ubuntu-24.04-x86_64-systemd"
SOURCE_COMMIT = "a" * 40


def _compatibility_from_catalog(catalog: dict[str, Any], group_id: str) -> dict[str, Any]:
    groups = catalog["compatibilityGroups"]
    group = next(row for row in groups if row["groupId"] == group_id)
    return {
        key: group[key] for key in ("groupId", "groupVersion", "contractApiVersion", "wireApiVersion", "contractLock")
    }


def test_native_control_manifests_bind_fixed_readiness_and_preserve_c11_contracts(tmp_path: Path) -> None:
    """Build signed manifests from the exact C11 catalog and validate the existing v2 schema."""
    catalog_raw = CATALOG_PATH.read_bytes()
    assert hashlib.sha256(catalog_raw).hexdigest() == C11_CATALOG_SHA256
    catalog = json.loads(catalog_raw.decode("utf-8", "strict"))

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
            catalog_path=CATALOG_PATH,
            catalog_sha256=C11_CATALOG_SHA256,
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
        assert validate_document(manifest_path, CATALOG_PATH.parent, "manifest") == []


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
