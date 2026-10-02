#!/usr/bin/env python3
"""Build target-specific native component payloads and release manifests.

The command consumes Cargo package metadata from this repository checkout and
never resolves a component version from a tag or mutable sibling checkout.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from pathlib import PurePosixPath
from typing import Any

if __package__:
    from .component_artifacts import (
        ComponentArtifactError,
        _component_target,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
    )
else:
    from component_artifacts import (
        ComponentArtifactError,
        _component_target,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
    )


PLATFORM_REPOSITORY = "https://github.com/DoHorizon-AI/Cyrene-Platform"
PLATFORM_WORKFLOW = "DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml"
LOCK_RELATIVE_PATH = "tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json"
TARGET = {
    "os": "linux",
    "osVersion": "24.04",
    "distribution": "ubuntu",
    "distributionVersion": "24.04",
    "architecture": "x86_64",
    "abi": "glibc-2.39",
    "runtime": "systemd",
}

COMPONENTS = (
    {"id": "cyrene-linux-sys-adapter", "package": "cyrene-linux-sys-adapter", "binary": "cyrene-linux-sys-adapter"},
    {"id": "cyrene-nvidia-adapter", "package": "cyrene-nvidia-adapter", "binary": "cyrene-nvidia-adapter"},
    {"id": "cyrene-sandboxd", "package": "cyrene-sandboxd", "binary": "cyrene-sandboxd"},
    {"id": "cyrene-kernel", "package": "cyrene-kernel", "binary": "cyrene-kernel"},
    {"id": "cy-node-agent", "package": "cy-node-agent", "binary": "cy-node-agent"},
    {"id": "cy-runtime-agent", "package": "cy-runtime-agent", "binary": "cy-runtime-agent"},
    {"id": "cy-workspace-relay", "package": "cy-workspace-relay-host", "binary": "cy-workspace-relay-host"},
    {"id": "cy-workspace-connector", "package": "cy-workspace-connector-host", "binary": "cy-workspace-connector-host"},
    {"id": "cy-workspace-sidecar", "package": "cy-workspace-sidecar", "binary": "cy-workspace-sidecar"},
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
    "cy-workspace-relay": "cy-workspace-relay.service",
    "cy-workspace-connector": "cy-workspace-connector.service",
    "cy-workspace-sidecar": "cy-workspace-sidecar.service",
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


def _compatibility(repository: Path, source_commit: str, group: dict[str, Any]) -> dict[str, Any]:
    lock_path = repository / LOCK_RELATIVE_PATH
    if lock_path.is_symlink() or not lock_path.is_file():
        raise ComponentArtifactError(f"tracked V2 contract lock is missing: {lock_path}")
    raw = lock_path.read_bytes()
    lock = json.loads(raw)
    if lock.get("wireApiVersion") != group.get("wireApiVersion"):
        raise ComponentArtifactError("tracked V2 lock wire API does not match the trusted compatibility group")
    contract_api_version = lock.get("contractApiVersion")
    if not isinstance(contract_api_version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", contract_api_version):
        raise ComponentArtifactError("tracked V2 lock has an invalid contractApiVersion")
    if contract_api_version != group.get("contractApiVersion"):
        raise ComponentArtifactError("tracked V2 lock contract API does not match the trusted compatibility group")
    return {
        "groupId": group["groupId"],
        "contractApiVersion": contract_api_version,
        "wireApiVersion": lock["wireApiVersion"],
        "contractLock": {
            "repository": "DoHorizon-AI/Cyrene-Platform",
            "commit": source_commit,
            "path": LOCK_RELATIVE_PATH,
            "sha256": "sha256:" + hashlib.sha256(raw).hexdigest(),
        },
    }


def _verified_product_contract_files(repository: Path, bundle_root: Path) -> dict[str, Path]:
    """Verify the generated Product bundle and return its exact regular files."""
    if bundle_root.is_symlink() or not bundle_root.is_dir():
        raise ComponentArtifactError(f"Product contract bundle root is missing or unsafe: {bundle_root}")
    lock_path = repository / LOCK_RELATIVE_PATH
    if lock_path.is_symlink() or not lock_path.is_file():
        raise ComponentArtifactError(f"tracked Product contract lock is missing or unsafe: {lock_path}")
    lock = json.loads(lock_path.read_text(encoding="utf-8"))

    def safe_file(relative: str) -> Path:
        pure = PurePosixPath(relative)
        if (
            not relative
            or "\\" in relative
            or pure.is_absolute()
            or pure.as_posix() != relative
            or any(part in {"", ".", ".."} for part in pure.parts)
        ):
            raise ComponentArtifactError("tracked Product contract lock contains an unsafe relative path")
        current = bundle_root
        for part in pure.parts[:-1]:
            current = current / part
            info = current.lstat()
            if not stat.S_ISDIR(info.st_mode):
                raise ComponentArtifactError(f"Product contract bundle traverses an unsafe directory: {relative}")
        path = current / pure.parts[-1]
        if not stat.S_ISREG(path.lstat().st_mode):
            raise ComponentArtifactError(f"Product contract bundle input is not a regular file: {relative}")
        path.resolve(strict=True).relative_to(bundle_root.resolve(strict=True))
        return path

    manifest_path = safe_file(lock["bundle"]["manifestPath"])
    policy_path = safe_file(lock["policy"]["bundlePath"])
    manifest_raw = manifest_path.read_bytes()
    if hashlib.sha256(manifest_raw).hexdigest() != lock["bundle"]["manifestSha256"]:
        raise ComponentArtifactError("Product contract bundle manifest does not match the tracked lock")
    manifest = json.loads(manifest_raw)
    if (
        manifest.get("formatVersion") != lock.get("formatVersion")
        or manifest.get("wireApiVersion") != lock.get("wireApiVersion")
        or manifest.get("owners") != lock.get("owners")
    ):
        raise ComponentArtifactError("Product contract bundle identity differs from the tracked lock")
    policy_raw = policy_path.read_bytes()
    if hashlib.sha256(policy_raw).hexdigest() != lock["policy"]["sha256"]:
        raise ComponentArtifactError("Product authorization policy does not match the tracked lock")
    entries = manifest.get("files")
    if not isinstance(entries, list) or not entries:
        raise ComponentArtifactError("Product contract bundle manifest has no files array")
    expected: dict[str, Path] = {manifest_path.name: manifest_path}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ComponentArtifactError("Product contract bundle file entry is malformed")
        relative = entry.get("path")
        digest = entry.get("sha256")
        if (
            not isinstance(relative, str)
            or not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
            or relative in expected
        ):
            raise ComponentArtifactError("Product contract bundle manifest contains an unsafe or duplicate path")
        path = safe_file(relative)
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ComponentArtifactError(f"Product contract bundle file digest differs: {relative}")
        expected[relative] = path

    policy_relative = lock["policy"]["bundlePath"]
    if expected.get(policy_relative) != policy_path:
        raise ComponentArtifactError("tracked policy path is absent from the Product bundle manifest")
    actual: set[str] = set()
    for current, directory_names, file_names in os.walk(bundle_root, followlinks=False):
        current_path = Path(current)
        for name in directory_names:
            path = current_path / name
            if path.is_symlink() or not stat.S_ISDIR(path.lstat().st_mode):
                raise ComponentArtifactError(f"Product contract bundle contains an unsafe directory: {path}")
        for name in file_names:
            path = current_path / name
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode):
                raise ComponentArtifactError(f"Product contract bundle contains a non-regular file: {path}")
            actual.add(path.relative_to(bundle_root).as_posix())
    if actual != set(expected):
        raise ComponentArtifactError("Product contract bundle contains missing or unlisted files")
    return expected


def _copy_product_contract_bundle(repository: Path, source_root: Path, payload_root: Path) -> None:
    """Place the verified V2 closure next to the host binary in its release."""
    files = _verified_product_contract_files(repository, source_root)
    destination = payload_root / "share" / "cyrene" / "product-contracts"
    if destination.exists() or destination.is_symlink():
        raise ComponentArtifactError("native payload already contains a Product contract directory")
    for relative, source in files.items():
        target = destination.joinpath(*Path(relative).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)


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
    product_contracts_root: Path,
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
            raise ComponentArtifactError(f"component is missing from the pinned static catalog: {component['id']}")
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
        if component["id"] in {"cy-workspace-connector", "cy-workspace-web-bff"}:
            _copy_product_contract_bundle(repository, product_contracts_root, payload_root)
        _copy_systemd_unit_payload(repository, catalog_component, payload_root)
        archive = artifacts_dir / f"{component['id']}-linux-ubuntu-24.04-x86_64-systemd.tar.gz"
        _write_deterministic_tar_gz(payload_root, archive)
        subject_name = archive.name
        artifact = {
            "kind": "native-binary",
            "format": "tar.gz",
            "entrypoint": f"bin/{component['binary']}",
        }
        _component_target(catalog, component["id"], artifact, TARGET)
        descriptor: dict[str, Any] = {
            "schemaVersion": 1,
            "releaseId": release_id,
            "componentId": component["id"],
            "version": package["version"],
            "channel": channel,
            "target": TARGET,
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
        if component_compatibility is not None:
            descriptor["compatibility"] = _compatibility(repository, source_commit, component_compatibility)
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
    parser.add_argument("--product-contracts-root", type=Path, required=True)
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
            product_contracts_root=args.product_contracts_root.resolve(),
        )
    except (ComponentArtifactError, OSError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"native component build failed: {error}", file=sys.stderr)
        return 2
    for path in manifests:
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
