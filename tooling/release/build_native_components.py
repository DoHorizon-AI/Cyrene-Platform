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
import platform
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
        _verify_contract_lock,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
        trusted_catalog_compatibility,
    )
else:
    from component_artifacts import (
        ComponentArtifactError,
        _component_target,
        _verify_contract_lock,
        _write_deterministic_tar_gz,
        create_manifest,
        manifest_asset_name,
        trusted_catalog_compatibility,
    )


PLATFORM_REPOSITORY = "https://github.com/DoHorizon-AI/Cyrene-Platform"
PLATFORM_WORKFLOW = "DoHorizon-AI/Cyrene-Platform/.github/workflows/component-release.yml"
TARGETS = {
    "linux-ubuntu-22.04-x86_64-systemd": {
        "os": "linux",
        "osVersion": "22.04",
        "distribution": "ubuntu",
        "distributionVersion": "22.04",
        "architecture": "x86_64",
        "abi": "glibc-2.35",
        "runtime": "systemd",
    },
    "linux-ubuntu-24.04-x86_64-systemd": {
        "os": "linux",
        "osVersion": "24.04",
        "distribution": "ubuntu",
        "distributionVersion": "24.04",
        "architecture": "x86_64",
        "abi": "glibc-2.39",
        "runtime": "systemd",
    },
}

COMPONENTS = (
    {"id": "cy-package-runtime", "package": "cy-package-runtime", "binary": "cy-package-runtime"},
    {"id": "cyrene-linux-sys-adapter", "package": "cyrene-linux-sys-adapter", "binary": "cyrene-linux-sys-adapter"},
    {"id": "cyrene-nvidia-adapter", "package": "cyrene-nvidia-adapter", "binary": "cyrene-nvidia-adapter"},
    {"id": "cyrene-sandboxd", "package": "cyrene-sandboxd", "binary": "cyrene-sandboxd"},
    {"id": "cyrene-kernel", "package": "cyrene-kernel", "binary": "cyrene-kernel"},
    {"id": "cy-node-agent", "package": "cy-node-agent", "binary": "cy-node-agent"},
    {"id": "cy-runtime-agent", "package": "cy-runtime-agent", "binary": "cy-runtime-agent"},
    {
        "id": "cy-workspace-authority-host",
        "package": "cy-workspace-authority-host",
        "binary": "cy-workspace-authority-host",
    },
    {"id": "cy-workspace-web-bff", "package": "cy-workspace-web-bff", "binary": "cy-workspace-web-bff"},
    {"id": "cyrene-runtime-maintenance", "package": "cy-runtime-maintenance", "binary": "cyrene-runtime-maintenance"},
)

# Host units travel inside the same immutable artifact as their executable.
# Installation is performed separately from this builder and may only install
# a missing unit after the active component release has been verified.
SYSTEMD_UNIT_COMPONENTS = {
    "cy-package-runtime": "cyrene-package-runtime.service",
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
PACKAGE_RUNTIME_ID = "cy-package-runtime"
PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID = "package-runtime-native-v1"
PACKAGE_RUNTIME_COMPATIBILITY = {
    "groupId": PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID,
    "groupVersion": "2",
    "contractApiVersion": "0.1.0",
    "wireApiVersion": "cyrene.runtime-maintenance.binding-operations.v1",
    "contractLock": {
        "repository": "DoHorizon-AI/Cyrene-Workspace",
        "path": "governance/package-runtime-protocols-v1.lock.json",
        "commit": "83e9a8e0a6db5ac8fef9fc9472e47f6ee9321bd8",
        "sha256": "sha256:fcfb13fe19fa2db8055c318c903f00e4f65a1d2e4c33700e44b08e694612a267",
    },
}
ATTACHED_NATIVE_BINARIES = {
    "cy-workspace-authority-host": (
        ("cy-workspace-authority-host", "cy-workspace-authority-admin"),
        ("cy-workspace-postgres-storage", "cy-workspace-storage-migrator"),
        ("cy-workspace-postgres-storage", "cy-workspace-device-ca-admin"),
        ("cy-workspace-postgres-storage", "cy-workspace-directory-admin"),
    ),
}


def _assert_build_host_matches_target(target_id: str) -> None:
    """Reject a native build host that cannot truthfully claim the catalog ABI.

    Args:
        target_id: Exact Platform native target profile selected by the release job.
    Raises:
        ComponentArtifactError: If OS, Ubuntu release, architecture, or glibc differs.

    中文：确认构建机的发行版、架构和 glibc 与发布目标完全一致。
    """
    target = TARGETS.get(target_id)
    if target is None:
        raise ComponentArtifactError(f"unsupported native target: {target_id}")

    os_release = platform.freedesktop_os_release()
    if os_release.get("ID") != target["distribution"] or os_release.get("VERSION_ID") != target["osVersion"]:
        raise ComponentArtifactError(
            f"native target {target_id} requires {target['distribution']} {target['osVersion']}"
        )
    if platform.machine().lower() not in {target["architecture"], "amd64"}:
        raise ComponentArtifactError(f"native target {target_id} requires {target['architecture']}")

    libc_name, libc_version = platform.libc_ver()
    if libc_name != "glibc" and hasattr(os, "confstr"):
        libc_version = (os.confstr("CS_GNU_LIBC_VERSION") or "").removeprefix("glibc ")
    if f"glibc-{libc_version}" != target["abi"]:
        raise ComponentArtifactError(
            f"native target {target_id} requires {target['abi']}, got {libc_name}-{libc_version}"
        )


def _require_package_runtime_catalog_target(catalog: dict[str, Any], target_id: str) -> dict[str, Any]:
    """Require the new daemon's exact publisher, unit, target, and ABI authorization.

    Args:
        catalog: SHA-256-pinned Workspace component catalog.
        target_id: Exact Ubuntu 22.04 or 24.04 native target profile.
    Returns:
        The uniquely declared Platform-owned package-runtime component row.
    Raises:
        ComponentArtifactError: If the component or target is absent, duplicated, or unsupported.

    中文：缺少精确目录授权时拒绝静默跳过 package runtime 的发布。
    """
    expected_target = TARGETS.get(target_id)
    if expected_target is None:
        raise ComponentArtifactError(f"unsupported package-runtime target: {target_id}")

    components = catalog.get("components")
    rows = [row for row in components or [] if isinstance(row, dict) and row.get("componentId") == PACKAGE_RUNTIME_ID]
    if len(rows) != 1:
        raise ComponentArtifactError("trusted component catalog must declare cy-package-runtime exactly once")
    component = rows[0]
    if component.get("publisher") != "DoHorizon-AI/Cyrene-Platform":
        raise ComponentArtifactError("cy-package-runtime publisher does not match Cyrene-Platform")

    target_rows = [row for row in catalog.get("targets", []) if isinstance(row, dict) and row.get("id") == target_id]
    if len(target_rows) != 1 or target_rows[0].get("target") != expected_target:
        raise ComponentArtifactError(f"trusted catalog target does not match native profile: {target_id}")

    declarations = [
        row
        for row in component.get("targets", [])
        if isinstance(row, dict) and row.get("targetId") == target_id and row.get("artifactKind") == "native-binary"
    ]
    if len(declarations) != 1 or declarations[0].get("support") != "supported":
        raise ComponentArtifactError(
            f"trusted catalog must authorize cy-package-runtime native-binary target: {target_id}"
        )
    _component_target(catalog, PACKAGE_RUNTIME_ID, {"kind": "native-binary"}, expected_target)

    restart = component.get("restart")
    if not isinstance(restart, dict):
        raise ComponentArtifactError("cy-package-runtime catalog restart policy is malformed")
    expected_unit = SYSTEMD_UNIT_COMPONENTS[PACKAGE_RUNTIME_ID]
    unit_values = [component.get("systemdUnit"), restart.get("unit")]
    declared_units = [value for value in unit_values if value is not None]
    if not declared_units or any(value != expected_unit for value in declared_units):
        raise ComponentArtifactError(f"cy-package-runtime catalog must bind the exact unit {expected_unit}")
    return component


def _trusted_package_runtime_compatibility(group: dict[str, Any]) -> dict[str, Any]:
    """Accept only the frozen Workspace Package Runtime lock and API identity.

    Args:
        group: Compatibility group from the SHA-256-pinned Workspace catalog.
    Returns:
        The exact compatibility fields to bind into this component manifest.
    Raises:
        ComponentArtifactError: If the group or its immutable lock differs from the
            producer's frozen Package Runtime contract.

    中文：仅接受 Package Runtime 自己的固定 Workspace 协议锁，不借用连接协议锁。
    """
    for key in ("groupId", "groupVersion", "contractApiVersion", "wireApiVersion"):
        if group.get(key) != PACKAGE_RUNTIME_COMPATIBILITY[key]:
            raise ComponentArtifactError(f"trusted package-runtime compatibility {key} does not match the frozen protocol")

    lock = group.get("contractLock")
    expected_lock = PACKAGE_RUNTIME_COMPATIBILITY["contractLock"]
    if not isinstance(lock, dict) or lock != expected_lock:
        raise ComponentArtifactError("trusted package-runtime contractLock does not match the frozen protocol lock")

    # Re-fetch the exact pinned lock bytes; the signed catalog's hash is not a substitute for the lock digest.
    # 即使目录本身已固定，仍核对独立协议锁的实际字节摘要。
    _verify_contract_lock({"contractLock": lock})
    return {
        "groupId": PACKAGE_RUNTIME_COMPATIBILITY["groupId"],
        "groupVersion": PACKAGE_RUNTIME_COMPATIBILITY["groupVersion"],
        "contractApiVersion": PACKAGE_RUNTIME_COMPATIBILITY["contractApiVersion"],
        "wireApiVersion": PACKAGE_RUNTIME_COMPATIBILITY["wireApiVersion"],
        "contractLock": dict(lock),
    }


def _trusted_catalog_compatibility_for_component(
    catalog_path: Path,
    catalog: dict[str, Any],
    component: dict[str, Any],
) -> dict[str, Any] | None:
    """Route each frozen compatibility category through its own pinned lock.

    Args:
        catalog_path: SHA-256-pinned catalog path used by the existing connection helper.
        catalog: Parsed trusted component catalog.
        component: Platform component row selected from that catalog.
    Returns:
        Trusted compatibility metadata, or ``None`` when the component has no group.
    Raises:
        ComponentArtifactError: If the Package Runtime group is duplicated or its
            fixed identity/lock fails verification.

    中文：保留 Workspace connection lock 的既有校验，只为固定 runtime 组增加独立分支。
    """
    if component.get("compatibilityGroup") != PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID:
        return trusted_catalog_compatibility(catalog_path, catalog, component)

    groups = catalog.get("compatibilityGroups")
    if not isinstance(groups, list):
        raise ComponentArtifactError("trusted catalog has no compatibilityGroups array")
    matches = [
        row
        for row in groups
        if isinstance(row, dict) and row.get("groupId") == PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID
    ]
    if len(matches) != 1:
        raise ComponentArtifactError("trusted catalog must declare package-runtime-native-v1 exactly once")
    return _trusted_package_runtime_compatibility(matches[0])


def _validate_native_binary_abi(binary_path: Path, target_id: str, *, repository: Path) -> None:
    """Inspect the compiled ELF architecture and maximum referenced GLIBC version.

    Args:
        binary_path: Cargo-produced binary being added to the immutable payload.
        target_id: Exact Platform native target profile selected by the release job.
        repository: Checked-out Platform source root used to execute readelf.
    Raises:
        ComponentArtifactError: If the binary cannot be identified as compatible with the target.

    中文：读取真实 ELF 头和符号版本，禁止将高版本 glibc 二进制标成较低 ABI。
    """
    target = TARGETS.get(target_id)
    if target is None:
        raise ComponentArtifactError(f"unsupported native target: {target_id}")

    header = _run(["readelf", "--file-header", str(binary_path)], cwd=repository)
    if not re.search(r"^\s*Class:\s+ELF64\s*$", header, flags=re.MULTILINE) or not re.search(
        r"^\s*Machine:\s+Advanced Micro Devices X86-64\s*$", header, flags=re.MULTILINE
    ):
        raise ComponentArtifactError(f"native binary is not an x86_64 ELF64 executable: {binary_path}")

    version_info = _run(["readelf", "--version-info", "--wide", str(binary_path)], cwd=repository)
    if "GLIBC_PRIVATE" in version_info:
        raise ComponentArtifactError(f"native binary requires private GLIBC symbols: {binary_path}")
    required_versions = [
        tuple(int(part or 0) for part in match.groups()[:2])
        for match in re.finditer(r"\bGLIBC_(\d+)\.(\d+)(?:\.(\d+))?\b", version_info)
    ]
    if not required_versions:
        dynamic_info = _run(["readelf", "--dynamic", str(binary_path)], cwd=repository)
        if "There is no dynamic section" not in dynamic_info:
            raise ComponentArtifactError(f"native binary GLIBC ABI could not be established: {binary_path}")
        return

    required_max = max(required_versions)
    abi_match = re.fullmatch(r"glibc-(\d+)\.(\d+)", target["abi"])
    if abi_match is None:
        raise ComponentArtifactError(f"native target ABI is unsupported: {target['abi']}")
    target_max = tuple(int(part) for part in abi_match.groups())
    if required_max > target_max:
        required_text = ".".join(str(part) for part in required_max)
        raise ComponentArtifactError(f"native binary requires GLIBC_{required_text}, exceeding target {target['abi']}")
    if "GLIBC_ABI_DT_RELR" in version_info and target_max < (2, 36):
        raise ComponentArtifactError(f"native binary uses GLIBC_ABI_DT_RELR unsupported by target {target['abi']}")


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


def _copy_attached_admin_binaries(
    repository: Path,
    packages: dict[str, dict[str, Any]],
    component_id: str,
    target_id: str,
    payload_root: Path,
) -> list[str]:
    """Build and stage explicitly bound authority admin executables.

    These tools share the authority host's signed native archive and source
    provenance, but remain attachments rather than new services/components.
    They are only built here; no tool is executed by the producer.

    Args:
        repository: Exact Platform checkout named by the source receipt.
        packages: Locked Cargo metadata from that checkout.
        component_id: Native component whose signed payload receives the tools.
        target_id: Exact Ubuntu target profile selected by the release matrix.
        payload_root: Fresh component payload directory.
    Returns:
        The relative paths of the copied executable attachments.
    Raises:
        ComponentArtifactError: If Cargo metadata, build output, or payload paths
            differ from the fixed authority tool inventory.

    中文：将 authority 管理工具以相同源码、目标和签名归档交付，但不创建新服务。
    """

    tools = ATTACHED_NATIVE_BINARIES.get(component_id, ())
    if not tools:
        return []

    bin_directory = payload_root / "bin"
    if bin_directory.is_symlink() or not bin_directory.is_dir():
        raise ComponentArtifactError(f"native executable payload directory is unsafe: {bin_directory}")

    target_directory = repository / "target/x86_64-unknown-linux-gnu/release"
    checked_tools: list[tuple[str, str]] = []
    seen_binaries: set[str] = set()
    for package_name, binary_name in tools:
        if not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", package_name) or not re.fullmatch(
            r"[a-z][a-z0-9-]{0,63}", binary_name
        ):
            raise ComponentArtifactError("attached native executable declaration has an unsafe name")
        if binary_name in seen_binaries:
            raise ComponentArtifactError(f"attached native executable is duplicated: {binary_name}")
        seen_binaries.add(binary_name)

        package = packages.get(package_name)
        if not isinstance(package, dict):
            raise ComponentArtifactError(f"Cargo package is missing for attached executable: {package_name}")
        binary_targets = [row for row in package.get("targets", []) if "bin" in row.get("kind", [])]
        if not any(row.get("name") == binary_name for row in binary_targets):
            raise ComponentArtifactError(f"Cargo package {package_name} has no binary target {binary_name}")
        checked_tools.append((package_name, binary_name))

    copied: list[str] = []
    for package_name, binary_name in checked_tools:
        _run(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--package",
                package_name,
                "--bin",
                binary_name,
            ],
            cwd=repository,
        )
        binary_path = target_directory / binary_name
        if binary_path.is_symlink() or not binary_path.is_file():
            raise ComponentArtifactError(f"built attached executable is missing or unsafe: {binary_path}")
        _validate_native_binary_abi(binary_path, target_id, repository=repository)

        destination = bin_directory / binary_name
        if destination.exists() or destination.is_symlink():
            raise ComponentArtifactError(f"attached executable payload destination already exists: {destination}")
        shutil.copyfile(binary_path, destination)
        destination.chmod(0o755)
        copied.append(f"bin/{binary_name}")
    return copied


def _bind_executable_files(artifact: dict[str, Any], attached_paths: list[str]) -> None:
    """Mark explicit signed native payload paths executable for the consumer.

    Args:
        artifact: Native descriptor artifact object with its primary entrypoint.
        attached_paths: Additional executable paths copied from this source tree.
    Raises:
        ComponentArtifactError: If the generated executable inventory is ambiguous.

    中文：只声明主入口与构建器显式附属工具为可执行文件。
    """

    if not attached_paths:
        return
    entrypoint = artifact.get("entrypoint")
    if not isinstance(entrypoint, str) or any(not isinstance(path, str) for path in attached_paths):
        raise ComponentArtifactError("native executable inventory contains an invalid path")
    executable_files = [entrypoint, *attached_paths]
    if len(set(executable_files)) != len(executable_files):
        raise ComponentArtifactError("native executable inventory contains duplicate paths")
    artifact["executableFiles"] = executable_files


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
    _assert_build_host_matches_target(target_id)
    _require_package_runtime_catalog_target(catalog, target_id)

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
                row
                for row in catalog_component.get("targets", [])
                if isinstance(row, dict)
                and row.get("targetId") == target_id
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
        _validate_native_binary_abi(binary_path, target_id, repository=repository)
        component_dir = output / "staging" / component["id"]
        payload_root = component_dir / "payload"
        (payload_root / "bin").mkdir(parents=True)
        target_binary = payload_root / "bin" / component["binary"]
        target_binary.write_bytes(binary_path.read_bytes())
        target_binary.chmod(0o755)
        attached_executables = _copy_attached_admin_binaries(
            repository,
            packages,
            component["id"],
            target_id,
            payload_root,
        )
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
        _bind_executable_files(artifact, attached_executables)
        _component_target(catalog, component["id"], artifact, target)
        protocol_version = catalog_component.get("protocolVersion")
        if protocol_version is None and isinstance(group_id, str):
            protocol_member = (
                next(
                    (
                        row
                        for row in component_compatibility.get("members", [])
                        if isinstance(row, dict) and row.get("componentId") == component["id"]
                    ),
                    None,
                )
                if isinstance(component_compatibility, dict)
                else None
            )
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
            descriptor["compatibility"] = _trusted_catalog_compatibility_for_component(
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
