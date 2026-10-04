#!/usr/bin/env python3
"""Build target-specific native component payloads and release manifests.

The command consumes Cargo package metadata from this repository checkout and
never resolves a component version from a tag or mutable sibling checkout.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any

if __package__:
    from .component_artifacts import (
        ComponentArtifactError,
        _component_target,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
        trusted_catalog_compatibility,
    )
else:
    from component_artifacts import (
        ComponentArtifactError,
        _component_target,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
        trusted_catalog_compatibility,
    )


PLATFORM_REPOSITORY = "https://github.com/DoHorizon-AI/Cyrene-Platform"
PLATFORM_WORKFLOW = "DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml"
TARGETS = {
    "linux-ubuntu-22.04-x86_64-systemd": {
        "os": "linux", "osVersion": "22.04", "distribution": "ubuntu",
        "distributionVersion": "22.04", "architecture": "x86_64", "abi": "glibc-2.35", "runtime": "systemd",
    },
    "linux-ubuntu-24.04-x86_64-systemd": {
        "os": "linux", "osVersion": "24.04", "distribution": "ubuntu",
        "distributionVersion": "24.04", "architecture": "x86_64", "abi": "glibc-2.39", "runtime": "systemd",
    },
}

COMPONENTS = (
    {"id": "cyrene-linux-sys-adapter", "package": "cyrene-linux-sys-adapter", "binary": "cyrene-linux-sys-adapter"},
    {"id": "cyrene-nvidia-adapter", "package": "cyrene-nvidia-adapter", "binary": "cyrene-nvidia-adapter"},
    {"id": "cyrene-sandboxd", "package": "cyrene-sandboxd", "binary": "cyrene-sandboxd"},
    {"id": "cyrene-kernel", "package": "cyrene-kernel", "binary": "cyrene-kernel"},
    {"id": "cy-node-agent", "package": "cy-node-agent", "binary": "cy-node-agent"},
    {"id": "cy-runtime-agent", "package": "cy-runtime-agent", "binary": "cy-runtime-agent"},
    {"id": "cy-workspace-authority-host", "package": "cy-workspace-authority-host", "binary": "cy-workspace-authority-host"},
    {"id": "cy-workspace-web-bff", "package": "cy-workspace-web-bff", "binary": "cy-workspace-web-bff"},
    {"id": "cyrene-runtime-maintenance", "package": "cy-runtime-maintenance", "binary": "cyrene-runtime-maintenance"},
)

# Host units travel inside the same immutable artifact as their executable.
# Installation is performed separately from this builder and may only install
# a missing unit after the active component release has been verified.
SYSTEMD_UNIT_COMPONENTS = {
    "cyrene-linux-sys-adapter": "cyrene-linux-sys-adapter.service",
    "cyrene-nvidia-adapter": "cyrene-nvidia-adapter.service",
    "cyrene-sandboxd": "cyrene-sandboxd.service",
    "cyrene-kernel": "cyrene-kernel.service",
    "cy-node-agent": "cy-node-agent.service",
    "cy-workspace-authority-host": "cyrene-workspace-authority.service",
    "cy-workspace-web-bff": "cy-workspace-web-bff.service",
    "cyrene-runtime-maintenance": "cyrene-runtime-maintenance.service",
}
WORKER_SCOPED_COMPONENT_UNITS = {"cy-runtime-agent": "cy-runtime-agent.service"}


def _run(command: list[str], *, cwd: Path) -> str:
    result = subprocess.run(command, cwd=cwd, check=True, capture_output=True, text=True)
    return result.stdout


def _cargo_packages(repository: Path) -> dict[str, dict[str, Any]]:
    raw = _run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=repository)
    document = json.loads(raw)
    return {package["name"]: package for package in document["packages"]}


def _copy_systemd_unit_payload(
    repository: Path,
    catalog_component: dict[str, Any],
    payload_root: Path,
) -> str | None:
    """Copy a catalog-matched host unit into an immutable component payload.

    Worker-scoped runtime agents intentionally have no fixed host unit even
    though the current catalog retains a legacy ``systemdUnit`` value. Their
    deployed Worker instances must be discovered by the runtime authority;
    absence of such discovery remains fail-closed at readiness time.
    """

    component_id = catalog_component.get("componentId")
    restart = catalog_component.get("restart")
    catalog_unit = catalog_component.get("systemdUnit")
    if not isinstance(catalog_unit, str) and isinstance(restart, dict):
        catalog_unit = restart.get("unit")

    expected_unit = SYSTEMD_UNIT_COMPONENTS.get(component_id)
    if expected_unit is None:
        worker_unit = WORKER_SCOPED_COMPONENT_UNITS.get(component_id)
        if worker_unit is not None and catalog_unit == worker_unit:
            return None
        raise ComponentArtifactError(f"component has no supported native systemd policy: {component_id}")
    if catalog_unit != expected_unit:
        raise ComponentArtifactError(
            f"catalog unit does not match the native payload policy for {component_id}: {catalog_unit!r}"
        )

    source = repository / "infrastructure" / "systemd" / expected_unit
    try:
        source_info = source.lstat()
    except OSError as error:
        raise ComponentArtifactError(f"native systemd unit source is missing for {component_id}: {source}") from error
    if not stat.S_ISREG(source_info.st_mode):
        raise ComponentArtifactError(f"native systemd unit source is not a regular file: {source}")

    relative_path = f"systemd/{expected_unit}"
    destination = payload_root / relative_path
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise ComponentArtifactError(f"native systemd unit payload destination already exists: {destination}")
    shutil.copyfile(source, destination)
    destination.chmod(0o644)
    return relative_path


def _copy_managed_runtime_helper(repository: Path, component_id: str, payload_root: Path) -> None:
    """Include the signed setup/projection helper with its maintenance authority.

    The helper is deliberately absent from other component archives so its
    installer authority stays with the Runtime Maintenance component.
    """

    if component_id != "cyrene-runtime-maintenance":
        return
    source = repository / "tooling/runtime/cyrene_managed_runtime.py"
    try:
        source_info = source.lstat()
    except OSError as error:
        raise ComponentArtifactError(f"managed runtime helper is missing: {source}") from error
    if not stat.S_ISREG(source_info.st_mode):
        raise ComponentArtifactError(f"managed runtime helper is not a regular file: {source}")
    destination = payload_root / "share/cyrene-managed-runtime/cyrene_managed_runtime.py"
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise ComponentArtifactError(f"managed runtime helper payload destination already exists: {destination}")
    shutil.copyfile(source, destination)
    destination.chmod(0o644)


def build(
    repository: Path,
    output: Path,
    *,
    channel: str,
    source_ref: str,
    source_commit: str,
    run_id: str,
    run_attempt: int,
    catalog_path: Path,
    catalog_sha256: str,
    target_id: str,
) -> list[Path]:
    if channel not in {"stable", "preview"}:
        raise ComponentArtifactError("channel must be stable or preview")
    if (channel == "preview" and source_ref != "refs/heads/develop") or (
        channel == "stable" and source_ref not in {"refs/heads/main", "refs/heads/release"}
    ):
        raise ComponentArtifactError("source ref is not permitted for the release channel")
    if not re.fullmatch(r"[0-9a-f]{40}([0-9a-f]{24})?", source_commit):
        raise ComponentArtifactError("source commit must be an exact full Git SHA")
    if _run(["git", "rev-parse", "HEAD"], cwd=repository).strip() != source_commit:
        raise ComponentArtifactError("checked out source does not match source commit")
    catalog_raw = catalog_path.read_bytes()
    if not re.fullmatch(r"[0-9a-f]{64}", catalog_sha256):
        raise ComponentArtifactError("catalog-sha256 must be 64 lowercase hexadecimal characters")
    if hashlib.sha256(catalog_raw).hexdigest() != catalog_sha256:
        raise ComponentArtifactError("trusted component catalog bytes do not match the pinned SHA-256")
    try:
        catalog = json.loads(catalog_raw.decode("utf-8", "strict"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"trusted component catalog is invalid JSON: {error}") from error
    if not isinstance(catalog, dict) or catalog.get("schemaVersion") != 1:
        raise ComponentArtifactError("trusted component catalog has an unsupported schemaVersion")
    compatibility_groups = catalog.get("compatibilityGroups")
    if not isinstance(compatibility_groups, list):
        raise ComponentArtifactError("trusted component catalog has no compatibilityGroups array")
    if run_attempt < 1 or not run_id.isdigit():
        raise ComponentArtifactError("workflow run identity is invalid")
    target = TARGETS.get(target_id)
    if target is None:
        raise ComponentArtifactError(f"unsupported native target: {target_id}")

    packages = _cargo_packages(repository)
    release_id = f"{channel}-{source_commit}"
    output.mkdir(parents=True, exist_ok=False)
    artifacts_dir = output / "artifacts"
    manifests_dir = output / "manifests"
    artifacts_dir.mkdir()
    manifests_dir.mkdir()
    manifest_paths: list[Path] = []

    for component in COMPONENTS:
        catalog_components = catalog.get("components")
        catalog_component = next(
            (
                row
                for row in catalog_components or []
                if isinstance(row, dict) and row.get("componentId") == component["id"]
            ),
            None,
        )
        if (
            not isinstance(catalog_component, dict)
            or catalog_component.get("publisher") != "DoHorizon-AI/Cyrene-Platform"
        ):
            continue
        dependencies = catalog_component.get("dependencies")
        restart = catalog_component.get("restart")
        if not isinstance(dependencies, list) or not isinstance(restart, dict):
            raise ComponentArtifactError(f"catalog dependencies/restart are malformed: {component['id']}")
        dependency_ids: set[str] = set()
        for dependency in dependencies:
            if not isinstance(dependency, dict) or set(dependency) - {"componentId", "versionRange"}:
                raise ComponentArtifactError(f"catalog dependency is malformed: {component['id']}")
            dependency_id = dependency.get("componentId")
            version_range = dependency.get("versionRange")
            if (
                not isinstance(dependency_id, str)
                or not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", dependency_id)
                or dependency_id in dependency_ids
                or (
                    version_range is not None
                    and (not isinstance(version_range, str) or not version_range.strip() or len(version_range) > 128)
                )
            ):
                raise ComponentArtifactError(f"catalog dependency is malformed: {component['id']}")
            dependency_ids.add(dependency_id)
        component_compatibility = None
        group_id = catalog_component.get("compatibilityGroup")
        if group_id is not None:
            component_compatibility = next(
                (row for row in compatibility_groups if isinstance(row, dict) and row.get("groupId") == group_id),
                None,
            )
            if not isinstance(component_compatibility, dict):
                raise ComponentArtifactError(f"component references an unknown compatibility group: {component['id']}")
        package = packages.get(component["package"])
        if not isinstance(package, dict):
            raise ComponentArtifactError(f"Cargo package is missing: {component['package']}")
        binary_targets = [target for target in package.get("targets", []) if "bin" in target.get("kind", [])]
        if not any(target.get("name") == component["binary"] for target in binary_targets):
            raise ComponentArtifactError(
                f"Cargo package {component['package']} has no binary target {component['binary']}"
            )
        target_declaration = next(
            (
                row for row in catalog_component.get("targets", [])
                if isinstance(row, dict) and row.get("targetId") == target_id
                and row.get("artifactKind") == "native-binary"
            ),
            None,
        )
        if not isinstance(target_declaration, dict) or target_declaration.get("support") != "supported":
            continue
        _run(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--package",
                component["package"],
                "--bin",
                component["binary"],
            ],
            cwd=repository,
        )
        binary_path = repository / "target/x86_64-unknown-linux-gnu/release" / component["binary"]
        if binary_path.is_symlink() or not binary_path.is_file():
            raise ComponentArtifactError(f"built binary is missing or unsafe: {binary_path}")
        component_dir = output / "staging" / component["id"]
        payload_root = component_dir / "payload"
        (payload_root / "bin").mkdir(parents=True)
        target_binary = payload_root / "bin" / component["binary"]
        target_binary.write_bytes(binary_path.read_bytes())
        target_binary.chmod(0o755)
        _copy_systemd_unit_payload(repository, catalog_component, payload_root)
        _copy_managed_runtime_helper(repository, component["id"], payload_root)
        archive = artifacts_dir / f"{component['id']}-{target_id}.tar.gz"
        _write_deterministic_tar_gz(payload_root, archive)
        subject_name = archive.name
        artifact = {
            "kind": "native-binary",
            "format": "tar.gz",
            "entrypoint": f"bin/{component['binary']}",
        }
        _component_target(catalog, component["id"], artifact, target)
        protocol_version = catalog_component.get("protocolVersion")
        if protocol_version is None and isinstance(group_id, str):
            protocol_member = next(
                (
                    row for row in component_compatibility.get("members", [])
                    if isinstance(row, dict) and row.get("componentId") == component["id"]
                ),
                None,
            ) if isinstance(component_compatibility, dict) else None
            protocol_version = protocol_member.get("protocolVersion") if isinstance(protocol_member, dict) else None
        is_v2 = isinstance(protocol_version, str) and bool(protocol_version)
        descriptor: dict[str, Any] = {
            "schemaVersion": 2 if is_v2 else 1,
            "releaseId": release_id,
            "componentId": component["id"],
            "version": package["version"],
            "channel": channel,
            "target": target,
            "artifact": artifact,
            "dependencies": dependencies,
            "restart": restart,
            "source": {"repository": PLATFORM_REPOSITORY, "ref": source_ref, "commit": source_commit},
            "provenance": {
                "attestation": {
                    "kind": "github-artifact-attestation",
                    "subjectName": subject_name,
                    "repository": "DoHorizon-AI/Cyrene-Platform",
                    "workflow": PLATFORM_WORKFLOW,
                    "predicateType": "https://slsa.dev/provenance/v1",
                    "run": {
                        "id": run_id,
                        "attempt": run_attempt,
                        "url": f"https://github.com/DoHorizon-AI/Cyrene-Platform/actions/runs/{run_id}/attempts/{run_attempt}",
                    },
                }
            },
        }
        if is_v2:
            descriptor["protocolVersion"] = protocol_version
        if component_compatibility is not None:
            descriptor["compatibility"] = trusted_catalog_compatibility(
                catalog_path,
                catalog,
                catalog_component,
            )
        descriptor_path = component_dir / "descriptor.json"
        descriptor_path.parent.mkdir(parents=True, exist_ok=True)
        descriptor_path.write_text(json.dumps(descriptor, indent=2) + "\n", encoding="utf-8")
        manifest_path = manifests_dir / manifest_asset_name(descriptor)
        create_manifest(descriptor_path, manifest_path, archive, payload_root)
        manifest_paths.append(manifest_path)

    return manifest_paths


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", type=int, required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--catalog-sha256", required=True)
    parser.add_argument("--target-id", choices=tuple(TARGETS), required=True)
    args = parser.parse_args()
    try:
        manifests = build(
            args.repository.resolve(),
            args.output.resolve(),
            channel=args.channel,
            source_ref=args.source_ref,
            source_commit=args.source_commit,
            run_id=args.run_id,
            run_attempt=args.run_attempt,
            catalog_path=args.catalog.resolve(),
            catalog_sha256=args.catalog_sha256,
            target_id=args.target_id,
        )
    except (ComponentArtifactError, OSError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"native component build failed: {error}", file=sys.stderr)
        return 2
    for path in manifests:
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
