"""Signed Workspace Authority admin executable producer checks.

Workspace Authority 管理工具的签名原生发布检查。
"""

from __future__ import annotations

import json
import stat
from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("rfc8785", reason="release-builder tests require tooling/release/requirements.txt")

from tooling.release import build_native_components as native
from tooling.release import component_artifacts as artifacts


AUTHORITY_COMPONENT = "cy-workspace-authority-host"
SOURCE_COMMIT = "a" * 40
ADMIN_BINS = (
    "cy-workspace-authority-admin",
    "cy-workspace-storage-migrator",
    "cy-workspace-device-ca-admin",
    "cy-workspace-directory-admin",
)
EXPECTED_EXECUTABLES = [
    "bin/cy-workspace-authority-host",
    *(f"bin/{name}" for name in ADMIN_BINS),
]


def _cargo_package_metadata(*, omit: str | None = None) -> dict[str, dict[str, object]]:
    packages: dict[str, dict[str, object]] = {}
    for package_name, binary_name in native.ATTACHED_NATIVE_BINARIES[AUTHORITY_COMPONENT]:
        targets = packages.setdefault(package_name, {"targets": []})["targets"]
        if binary_name != omit:
            targets.append({"name": binary_name, "kind": ["bin"]})
    return packages


def _write_fake_build_outputs(repository: Path) -> list[Path]:
    target = repository / "target/x86_64-unknown-linux-gnu/release"
    target.mkdir(parents=True)
    outputs: list[Path] = []
    for _package, binary in native.ATTACHED_NATIVE_BINARIES[AUTHORITY_COMPONENT]:
        path = target / binary
        path.write_bytes(f"locked build output for {binary}\n".encode())
        path.chmod(0o755)
        outputs.append(path)
    return outputs


@pytest.mark.parametrize("target_id", tuple(native.TARGETS))
def test_all_admin_bins_build_locked_for_the_exact_native_target_and_match_abi(tmp_path: Path, target_id: str) -> None:
    """Build and inspect each authority helper independently on both Ubuntu runners."""
    repository = tmp_path / "source"
    repository.mkdir()
    outputs = _write_fake_build_outputs(repository)
    payload = tmp_path / "payload"
    (payload / "bin").mkdir(parents=True)
    commands: list[list[str]] = []
    validations: list[tuple[Path, str, Path]] = []

    def record_build(command: list[str], *, cwd: Path) -> str:
        commands.append(command)
        assert cwd == repository
        return ""

    def record_abi(path: Path, checked_target: str, *, repository: Path) -> None:
        validations.append((path, checked_target, repository))

    with (
        patch.object(native, "_run", side_effect=record_build),
        patch.object(native, "_validate_native_binary_abi", side_effect=record_abi),
    ):
        copied = native._copy_attached_admin_binaries(
            repository,
            _cargo_package_metadata(),
            AUTHORITY_COMPONENT,
            target_id,
            payload,
        )

    expected_builds = [
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--package",
            package,
            "--bin",
            binary,
        ]
        for package, binary in native.ATTACHED_NATIVE_BINARIES[AUTHORITY_COMPONENT]
    ]
    assert commands == expected_builds
    assert copied == EXPECTED_EXECUTABLES[1:]
    assert validations == [(path, target_id, repository) for path in outputs]
    for relative, output in zip(copied, outputs, strict=True):
        installed = payload / relative
        assert installed.read_bytes() == output.read_bytes()
        assert stat.S_IMODE(installed.stat().st_mode) == 0o755


def test_attached_tool_inventory_is_preflighted_before_any_cargo_build(tmp_path: Path) -> None:
    """A missing real Cargo bin cannot leave a partial helper build set."""
    repository = tmp_path / "source"
    repository.mkdir()
    payload = tmp_path / "payload"
    (payload / "bin").mkdir(parents=True)

    with patch.object(native, "_run") as run:
        with pytest.raises(native.ComponentArtifactError, match="has no binary target"):
            native._copy_attached_admin_binaries(
                repository,
                _cargo_package_metadata(omit="cy-workspace-directory-admin"),
                AUTHORITY_COMPONENT,
                "linux-ubuntu-22.04-x86_64-systemd",
                payload,
            )

    run.assert_not_called()


def test_non_authority_components_get_no_attached_admin_tools(tmp_path: Path) -> None:
    """Authority maintenance commands do not become a new service or generic payload."""
    with patch.object(native, "_run") as run:
        assert (
            native._copy_attached_admin_binaries(
                tmp_path,
                {},
                "cyrene-runtime-maintenance",
                "linux-ubuntu-22.04-x86_64-systemd",
                tmp_path,
            )
            == []
        )
    run.assert_not_called()


def test_authority_helpers_are_fixed_package_bins_not_services() -> None:
    """Keep the control-host entrypoint and unit while pinning each real Cargo bin."""
    assert native.ATTACHED_NATIVE_BINARIES[AUTHORITY_COMPONENT] == (
        ("cy-workspace-authority-host", "cy-workspace-authority-admin"),
        ("cy-workspace-postgres-storage", "cy-workspace-storage-migrator"),
        ("cy-workspace-postgres-storage", "cy-workspace-device-ca-admin"),
        ("cy-workspace-postgres-storage", "cy-workspace-directory-admin"),
    )
    authority = next(component for component in native.COMPONENTS if component["id"] == AUTHORITY_COMPONENT)
    assert authority["binary"] == "cy-workspace-authority-host"
    assert native.SYSTEMD_UNIT_COMPONENTS[AUTHORITY_COMPONENT] == "cyrene-workspace-authority.service"


def test_executable_files_declaration_is_primary_then_four_fixed_helpers() -> None:
    """Only the authority main entrypoint and explicit helpers receive executable mode."""
    artifact: dict[str, object] = {
        "kind": "native-binary",
        "format": "tar.gz",
        "entrypoint": EXPECTED_EXECUTABLES[0],
    }

    native._bind_executable_files(artifact, EXPECTED_EXECUTABLES[1:])

    assert artifact["executableFiles"] == EXPECTED_EXECUTABLES
    legacy_artifact: dict[str, object] = {"entrypoint": "bin/legacy"}
    native._bind_executable_files(legacy_artifact, [])
    assert "executableFiles" not in legacy_artifact


def test_admin_bytes_and_executable_mode_are_bound_by_one_source_manifest(tmp_path: Path) -> None:
    """The helpers, main daemon, and unit share one source-pinned archive map."""
    payload = tmp_path / "payload"
    (payload / "bin").mkdir(parents=True)
    (payload / "systemd").mkdir()
    for relative in EXPECTED_EXECUTABLES:
        path = payload / relative
        path.write_bytes(f"signed source bytes:{relative}\n".encode())
        path.chmod(0o755)
    (payload / "systemd/cyrene-workspace-authority.service").write_bytes(b"[Service]\n")
    archive = tmp_path / "cy-workspace-authority-host-linux-ubuntu-22-04.tar.gz"
    artifacts._write_deterministic_tar_gz(payload, archive)

    artifact: dict[str, object] = {
        "kind": "native-binary",
        "format": "tar.gz",
        "entrypoint": EXPECTED_EXECUTABLES[0],
    }
    native._bind_executable_files(artifact, EXPECTED_EXECUTABLES[1:])
    descriptor = {
        "schemaVersion": 1,
        "releaseId": f"preview-{SOURCE_COMMIT}",
        "componentId": AUTHORITY_COMPONENT,
        "version": "0.1.0",
        "channel": "preview",
        "target": native.TARGETS["linux-ubuntu-22.04-x86_64-systemd"],
        "artifact": artifact,
        "dependencies": [],
        "restart": {"group": "single-service", "unit": "cyrene-workspace-authority.service"},
        "source": {
            "repository": native.PLATFORM_REPOSITORY,
            "ref": "refs/heads/develop",
            "commit": SOURCE_COMMIT,
        },
        "provenance": {
            "attestation": {
                "kind": "github-artifact-attestation",
                "uri": "https://github.com/DoHorizon-AI/Cyrene-Platform/attestations/123456789",
                "subjectName": archive.name,
                "repository": "DoHorizon-AI/Cyrene-Platform",
                "workflow": native.PLATFORM_WORKFLOW,
                "predicateType": artifacts.ATTESTATION_PREDICATE,
                "run": {
                    "id": "123456789",
                    "attempt": 1,
                    "url": "https://github.com/DoHorizon-AI/Cyrene-Platform/actions/runs/123456789/attempts/1",
                },
            }
        },
    }
    descriptor_path = tmp_path / "descriptor.json"
    descriptor_path.write_text(json.dumps(descriptor, indent=2) + "\n", encoding="utf-8")
    manifest_path = tmp_path / "manifest.json"
    manifest = artifacts.create_manifest(descriptor_path, manifest_path, archive, payload)
    verified = artifacts.verify_manifest(
        manifest_path,
        archive,
        verify_run=False,
        verify_attestation=False,
    )

    assert verified["source"]["commit"] == SOURCE_COMMIT
    assert verified["artifact"]["executableFiles"] == EXPECTED_EXECUTABLES
    assert set(verified["artifact"]["files"]) == {
        *EXPECTED_EXECUTABLES,
        "systemd/cyrene-workspace-authority.service",
    }
    assert artifacts._archive_files(archive) == manifest["artifact"]["files"]
