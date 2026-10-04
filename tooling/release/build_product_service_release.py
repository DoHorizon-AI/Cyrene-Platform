#!/usr/bin/env python3
"""Build one Product Python bundle from exact Workspace, Product, and SDK pins.

The helper delegates dependency resolution to the pinned Workspace packaging
implementation, then binds the Product's v2 owner contract closure into both
internal and public bundle digests.
该脚本按固定 Workspace、Product 与 SDK pin 构建单个 Product Python 发布包。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path, PurePosixPath
from typing import Any

from component_artifacts import (
    ComponentArtifactError,
    _archive_files,
    _component_target,
    _sha256_file,
    _write_deterministic_tar_gz,
    create_manifest,
    manifest_asset_name,
    verify_manifest,
)
from component_descriptor import _catalog_component
from product_contract_files import collect_product_contract_files


PLATFORM_REPOSITORY = "DoHorizon-AI/Cyrene-Platform"
WORKSPACE_REPOSITORY = "DoHorizon-AI/Cyrene-Workspace"
SERVICE_COMPONENTS = {
    "catalyst": "cyrene-catalyst",
    "yield": "cyrene-yield",
    "reactor": "cyrene-reactor",
    "exchange": "cyrene-exchange",
    "navigator": "cyrene-navigator",
}
NATIVE_PYTHON_TARGETS = {
    "linux-ubuntu-22.04-x86_64-python-3.12": {
        "osVersion": "22.04",
        "distributionVersion": "22.04",
        "abi": "glibc-2.35",
    },
    "linux-ubuntu-24.04-x86_64-python-3.12": {
        "osVersion": "24.04",
        "distributionVersion": "24.04",
        "abi": "glibc-2.39",
    },
}
NATIVE_PYTHON_VERSION = "3.12.14"
NATIVE_PYTHON_EXECUTABLE = "/opt/cyrene/python/3.12.14/bin/python3.12"
NATIVE_PYTHON_INPUT = "packaging/python-runtime.lock.json"
SDK_COMPONENT = "cyrene-runtime-maintenance-sdk"
SDK_WHEEL = "cyrene_runtime_maintenance-0.1.0-py3-none-any.whl"
SHA1 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class ProductReleaseError(ValueError):
    """Raised when a Product release input cannot be pinned and verified."""


def _run(arguments: list[str], *, cwd: Path | None = None) -> str:
    result = subprocess.run(arguments, cwd=cwd, check=True, capture_output=True, text=True)
    return result.stdout.strip()


def _uv_version_matches_locked_profile(output: str, expected_version: str, expected_target: str) -> bool:
    """Accept only the locked uv version and, when present, its exact target annotation."""

    return output in {
        f"uv {expected_version}",
        f"uv {expected_version} ({expected_target})",
    }


def _git_head(root: Path, expected_repository: str, label: str) -> str:
    if root.is_symlink() or not root.is_dir():
        raise ProductReleaseError(f"{label} checkout root is missing or unsafe: {root}")
    origin = _run(["git", "remote", "get-url", "origin"], cwd=root)
    expected = f"https://github.com/{expected_repository}.git"
    allowed_origins = {expected, expected.removesuffix(".git")}
    if origin not in allowed_origins:
        raise ProductReleaseError(f"{label} checkout origin does not match {expected_repository}")
    head = _run(["git", "rev-parse", "HEAD"], cwd=root)
    if _run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=root):
        raise ProductReleaseError(f"{label} exact-SHA checkout is dirty: {root}")
    return head


def _read_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ProductReleaseError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise ProductReleaseError(f"{label} must be a JSON object: {path}")
    return value


def _extract_sdk_bundle(fetch_result_path: Path, destination: Path, channel: str) -> dict[str, str]:
    result = _read_json(fetch_result_path, "verified SDK fetch result")
    manifest_path = Path(result.get("manifestPath", ""))
    archive_path = Path(result.get("artifactPath", ""))
    manifest = verify_manifest(
        manifest_path,
        archive_path,
        verify_run=False,
        verify_attestation=False,
    )
    if result.get("manifest") != manifest:
        raise ProductReleaseError("SDK fetch result manifest differs from the verified manifest file")
    index = result.get("index")
    if not isinstance(index, dict):
        raise ProductReleaseError("SDK fetch result must retain its verified component index")
    indexed_release = next(
        (
            row
            for row in index.get("releases", [])
            if isinstance(row, dict)
            and row.get("componentId") == manifest.get("componentId")
            and row.get("target") == manifest.get("target")
        ),
        None,
    )
    if (
        not isinstance(indexed_release, dict)
        or indexed_release.get("manifestDigest") != manifest.get("manifestDigest")
        or index.get("repository")
        != manifest.get("source", {}).get("repository", "").removeprefix("https://github.com/")
        or index.get("channel") != manifest.get("channel")
        or index.get("source") != manifest.get("source")
    ):
        raise ProductReleaseError("SDK manifest is not bound to the fetched index source and digest")
    if (
        manifest.get("componentId") != SDK_COMPONENT
        or manifest.get("version") != "0.1.0"
        or manifest.get("channel") != channel
        or manifest.get("source", {}).get("repository") != f"https://github.com/{PLATFORM_REPOSITORY}"
        or manifest.get("artifact", {}).get("kind") != "python-bundle"
        or manifest.get("target", {}).get("runtime") != "python:3.12"
    ):
        raise ProductReleaseError("verified SDK release manifest has the wrong component identity")
    artifact = manifest["artifact"]
    if _sha256_file(archive_path) != artifact["sha256"] or _archive_files(archive_path) != artifact["files"]:
        raise ProductReleaseError("verified SDK archive bytes do not match its manifest")

    destination.mkdir(parents=True, exist_ok=False)
    expected_names = {"sdk-release.json", SDK_WHEEL}
    if set(artifact["files"]) != expected_names:
        raise ProductReleaseError("SDK payload inventory must contain only sdk-release.json and the pinned wheel")
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            for member in archive.getmembers():
                relative = PurePosixPath(member.name)
                if (
                    not member.isfile()
                    or relative.is_absolute()
                    or relative.as_posix() != member.name
                    or any(part in {"", ".", ".."} for part in relative.parts)
                ):
                    raise ProductReleaseError(f"SDK archive contains an unsafe member: {member.name}")
                extracted = destination.joinpath(*relative.parts)
                extracted.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is None:
                    raise ProductReleaseError(f"SDK archive member has no content: {member.name}")
                extracted.write_bytes(source.read())
    except (OSError, tarfile.TarError) as error:
        raise ProductReleaseError(f"cannot unpack verified SDK archive: {error}") from error

    metadata = _read_json(destination / "sdk-release.json", "SDK wheel metadata")
    wheel_path = destination / SDK_WHEEL
    wheel_sha256 = hashlib.sha256(wheel_path.read_bytes()).hexdigest()
    expected_prefixed_digest = f"sha256:{wheel_sha256}"
    if (
        metadata.get("distribution") != "cyrene-runtime-maintenance"
        or metadata.get("version") != "0.1.0"
        or metadata.get("wheel") != SDK_WHEEL
        or metadata.get("wheelSha256") != expected_prefixed_digest
        or artifact["files"].get(SDK_WHEEL) != expected_prefixed_digest
    ):
        raise ProductReleaseError("SDK metadata, wheel hash, and outer manifest file map do not agree")
    wheel_context = destination.parent / "runtime-maintenance-wheel"
    wheel_context.mkdir()
    shutil.copyfile(wheel_path, wheel_context / SDK_WHEEL)
    return {
        "wheel": str(wheel_path),
        "buildContext": str(wheel_context),
        "wheelSha256": wheel_sha256,
        "manifestDigest": manifest["manifestDigest"],
        "artifactDigest": artifact["sha256"],
        "releaseId": manifest["releaseId"],
    }


def _native_python_profile(
    workspace_root: Path,
    catalog: dict[str, Any],
    component_id: str,
    target_profile: str,
    python_executable: Path,
    uv_executable: Path,
) -> tuple[dict[str, Any], Path, Path]:
    """Bind a native build host and interpreter to one catalog/lock profile."""
    expected = NATIVE_PYTHON_TARGETS.get(target_profile)
    if expected is None:
        raise ProductReleaseError(f"unsupported Product native Python profile: {target_profile}")
    target_row = next(
        (row for row in catalog.get("targets", []) if isinstance(row, dict) and row.get("id") == target_profile),
        None,
    )
    target = target_row.get("target") if isinstance(target_row, dict) else None
    if not isinstance(target, dict):
        raise ProductReleaseError(f"trusted catalog has no target profile: {target_profile}")
    if (
        target.get("os") != "linux"
        or target.get("distribution") != "ubuntu"
        or target.get("architecture") != "x86_64"
        or target.get("runtime") != "python:3.12"
        or any(target.get(key) != value for key, value in expected.items())
    ):
        raise ProductReleaseError(f"trusted catalog target does not match the native profile ID: {target_profile}")
    _component_target(catalog, component_id, {"kind": "python-bundle"}, target)

    release_lock = _read_json(workspace_root / "release-lock.json", "Workspace release lock")
    profiles = release_lock.get("nativePythonProfiles")
    profile = profiles.get(target_profile) if isinstance(profiles, dict) else None
    if not isinstance(profile, dict):
        raise ProductReleaseError(f"Workspace release lock has no native Python profile: {target_profile}")
    target_keys = (
        "os",
        "osVersion",
        "distribution",
        "distributionVersion",
        "architecture",
        "abi",
        "runtime",
    )
    if any(profile.get(key) != target.get(key) for key in target_keys):
        raise ProductReleaseError("Workspace native Python profile differs from the trusted catalog target")
    python_version = profile.get("pythonVersion")
    python_path = Path(python_executable)
    if (
        python_version != NATIVE_PYTHON_VERSION
        or profile.get("pythonExecutable") != NATIVE_PYTHON_EXECUTABLE
        or profile.get("pythonInput") != NATIVE_PYTHON_INPUT
        or not python_path.is_absolute()
    ):
        raise ProductReleaseError("native Python profile or staged interpreter path is invalid")
    try:
        resolved_python = python_path.resolve(strict=True)
    except OSError as error:
        raise ProductReleaseError(f"staged Python interpreter is missing: {python_path}") from error
    if not resolved_python.is_file() or not os.access(resolved_python, os.X_OK):
        raise ProductReleaseError(f"staged Python interpreter is not executable: {python_path}")
    actual_python_version = _run(
        [
            str(resolved_python),
            "-c",
            "import sys; print('.'.join(map(str, sys.version_info[:3])))",
        ]
    )
    if actual_python_version != python_version:
        raise ProductReleaseError(
            f"staged Python interpreter differs from the locked profile: expected {python_version}, got {actual_python_version}"
        )
    uv_path = Path(uv_executable)
    try:
        resolved_uv = uv_path.resolve(strict=True)
    except OSError as error:
        raise ProductReleaseError(f"pinned uv executable is missing: {uv_path}") from error
    if not resolved_uv.is_file() or not os.access(resolved_uv, os.X_OK):
        raise ProductReleaseError(f"pinned uv executable is not executable: {uv_path}")
    resolver = profile.get("wheelResolver")
    uv_version = resolver.get("version") if isinstance(resolver, dict) else None
    if not isinstance(uv_version, str) or not uv_version:
        raise ProductReleaseError("Workspace native Python profile has no pinned uv version")
    actual_uv_version = _run([str(resolved_uv), "--version"])
    expected_uv_target = f"{profile['architecture']}-unknown-linux-gnu"
    if not _uv_version_matches_locked_profile(actual_uv_version, uv_version, expected_uv_target):
        raise ProductReleaseError(
            f"uv executable differs from the locked profile: expected uv {uv_version}, got {actual_uv_version}"
        )

    actual_target = NATIVE_PYTHON_TARGETS[target_profile]
    os_release = platform.freedesktop_os_release()
    if os_release.get("ID") != "ubuntu" or os_release.get("VERSION_ID") != actual_target["osVersion"]:
        raise ProductReleaseError(
            f"native Product target {target_profile} requires Ubuntu {actual_target['osVersion']}"
        )
    if platform.machine().lower() not in {"x86_64", "amd64"}:
        raise ProductReleaseError(f"native Product target {target_profile} requires x86_64")
    libc_name, libc_version = platform.libc_ver()
    if libc_name != "glibc" and hasattr(os, "confstr"):
        libc_version = (os.confstr("CS_GNU_LIBC_VERSION") or "").removeprefix("glibc ")
    if f"glibc-{libc_version}" != actual_target["abi"]:
        raise ProductReleaseError(
            f"native Product target {target_profile} requires {actual_target['abi']}, got {libc_name}-{libc_version}"
        )
    return target, resolved_python, resolved_uv


def build_product_service_release(
    *,
    service: str,
    product_repository: Path,
    product_repository_id: str,
    product_commit: str,
    source_ref: str,
    channel: str,
    workspace_root: Path,
    workspace_builder_commit: str,
    release_tools_commit: str,
    release_tools_root: Path,
    catalog_path: Path,
    catalog_sha256: str,
    sdk_fetch_result: Path,
    target_profile: str,
    python_executable: Path,
    uv_executable: Path,
    output: Path,
    run_id: str,
    run_attempt: int,
) -> dict[str, Any]:
    if service not in SERVICE_COMPONENTS:
        raise ProductReleaseError(f"unsupported Product service: {service}")
    if not SHA1.fullmatch(product_commit) or not SHA1.fullmatch(workspace_builder_commit):
        raise ProductReleaseError("Product and Workspace commits must be full lowercase 40-character SHAs")
    if not SHA1.fullmatch(release_tools_commit):
        raise ProductReleaseError("release tooling commit must be a full lowercase 40-character SHA")
    if not SHA256.fullmatch(catalog_sha256):
        raise ProductReleaseError("catalog SHA-256 must be 64 lowercase hexadecimal characters")
    if (channel == "preview" and source_ref != "refs/heads/develop") or (
        channel == "stable" and source_ref not in {"refs/heads/main", "refs/heads/release"}
    ):
        raise ProductReleaseError("source ref is not permitted for the release channel")
    if not run_id.isdigit() or run_attempt < 1:
        raise ProductReleaseError("GitHub Actions run identity is invalid")
    if output.exists() or output.is_symlink():
        raise ProductReleaseError(f"output directory must not exist: {output}")

    actual_product_commit = _git_head(product_repository, product_repository_id, "Product")
    if actual_product_commit != product_commit:
        raise ProductReleaseError("checked out Product source does not match GITHUB_SHA")
    actual_workspace_commit = _git_head(workspace_root, WORKSPACE_REPOSITORY, "Workspace builder")
    if actual_workspace_commit != workspace_builder_commit:
        raise ProductReleaseError("Workspace packaging checkout does not match the pinned builder SHA")
    actual_tools_commit = _git_head(release_tools_root, PLATFORM_REPOSITORY, "Platform release tooling")
    if actual_tools_commit != release_tools_commit:
        raise ProductReleaseError("Platform release tooling checkout does not match the attested SDK source SHA")

    catalog_raw = catalog_path.read_bytes()
    if hashlib.sha256(catalog_raw).hexdigest() != catalog_sha256:
        raise ProductReleaseError("static component catalog does not match its pinned SHA-256")
    component_id = SERVICE_COMPONENTS[service]
    component, workflow, catalog = _catalog_component(
        catalog_path,
        catalog_sha256,
        component_id,
        product_repository_id,
    )
    if component.get("pythonBundleService") != service:
        raise ProductReleaseError("trusted catalog service mapping does not match this Product")
    target, python, uv = _native_python_profile(
        workspace_root,
        catalog,
        component_id,
        target_profile,
        python_executable,
        uv_executable,
    )

    output.mkdir(parents=True)
    sdk = _extract_sdk_bundle(sdk_fetch_result, output / "sdk-unpacked", channel)
    wheelhouse_root = output / "wheelhouse"
    bundle_root = output / "inner-bundle"
    package_builder = workspace_root / "packaging" / "prepare_service_wheelhouse.py"
    service_builder = workspace_root / "packaging" / "service_bundle.py"
    release_lock = workspace_root / "release-lock.json"
    for path in (package_builder, service_builder, release_lock):
        if path.is_symlink() or not path.is_file():
            raise ProductReleaseError(f"pinned Workspace builder input is missing or unsafe: {path}")
    _run(
        [
            str(python),
            str(package_builder),
            "--workspace-root",
            str(workspace_root),
            "--service",
            service,
            "--service-commit",
            product_commit,
            "--target-profile",
            target_profile,
            "--python-executable",
            str(python),
            "--uv",
            str(uv),
            "--output",
            str(wheelhouse_root),
            "--runtime-sdk-wheel",
            sdk["wheel"],
            "--runtime-sdk-version",
            "0.1.0",
            "--runtime-sdk-sha256",
            sdk["wheelSha256"],
            "--runtime-sdk-manifest-digest",
            sdk["manifestDigest"],
            "--runtime-sdk-artifact-digest",
            sdk["artifactDigest"],
        ],
        cwd=workspace_root,
    )
    _run(
        [
            str(python),
            str(service_builder),
            "build",
            "--wheelhouse",
            str(wheelhouse_root),
            "--release-lock",
            str(release_lock),
            "--output",
            str(bundle_root),
            "--arch",
            "amd64",
            "--target-profile",
            target_profile,
            "--python-executable",
            str(python),
            "--service",
            service,
            "--service-commit",
            product_commit,
        ],
        cwd=workspace_root,
    )
    service_output = bundle_root / service
    versions = [entry for entry in service_output.iterdir() if entry.is_dir() and not entry.is_symlink()]
    if len(versions) != 1:
        raise ProductReleaseError("Workspace service builder did not produce exactly one content-addressed bundle")
    inner_bundle = versions[0]
    service_module_path = workspace_root / "packaging" / "service_bundle.py"
    sys.path.insert(0, str(service_module_path.parent))
    try:
        import service_bundle  # type: ignore[import-not-found]
    except ImportError as error:
        raise ProductReleaseError(f"cannot load pinned Workspace service bundle verifier: {error}") from error
    inner_manifest = service_bundle.validate_bundle(inner_bundle, expected_service=service)

    inner_manifest = service_bundle.validate_bundle(inner_bundle, expected_service=service)
    if inner_manifest.get("schema_version") != 2:
        raise ProductReleaseError("new Product bundle releases require Workspace inner manifest schema v2")
    product_files = collect_product_contract_files(product_repository, service)

    payload_root = output / "payload"
    payload_root.mkdir()
    packaged_bundle = payload_root / "service-bundle"
    shutil.copytree(inner_bundle, packaged_bundle, symlinks=False)
    for relative, raw in product_files.items():
        target_path = payload_root.joinpath(*PurePosixPath(relative).parts)
        if target_path.exists() or target_path.is_symlink():
            raise ProductReleaseError(f"Product contract bundle path collides with runtime files: {relative}")
        target_path.parent.mkdir(parents=True, exist_ok=True)
        target_path.write_bytes(raw)
    artifact_dir = output / "artifacts"
    manifest_dir = output / "manifests"
    artifact_dir.mkdir()
    manifest_dir.mkdir()
    artifact_name = f"{component_id}-{target_profile}.tar.gz"
    artifact_path = artifact_dir / artifact_name
    _write_deterministic_tar_gz(payload_root, artifact_path)
    artifact = {"kind": "python-bundle", "format": "tar.gz"}
    _component_target(catalog, component_id, artifact, target)
    source_url = f"https://github.com/{product_repository_id}"
    run_url = f"{source_url}/actions/runs/{run_id}/attempts/{run_attempt}"
    descriptor = {
        "schemaVersion": 1,
        "releaseId": f"{channel}-{product_commit}",
        "componentId": component_id,
        "version": inner_manifest["version"],
        "channel": channel,
        "target": target,
        "artifact": artifact,
        "dependencies": component["dependencies"],
        "restart": component["restart"],
        "source": {"repository": source_url, "ref": source_ref, "commit": product_commit},
        "provenance": {
            "attestation": {
                "kind": "github-artifact-attestation",
                "subjectName": artifact_name,
                "repository": product_repository_id,
                "workflow": workflow,
                "predicateType": "https://slsa.dev/provenance/v1",
                "run": {"id": run_id, "attempt": run_attempt, "url": run_url},
            }
        },
    }
    if component_id in {
        "cyrene-catalyst",
        "cyrene-yield",
        "cyrene-reactor",
        "cyrene-exchange",
        "cyrene-echo",
        "cyrene-navigator",
    }:
        dependencies = {row["componentId"]: row.get("versionRange") for row in component["dependencies"]}
        expected = {
            "cyrene-runtime-maintenance-sdk": "=0.1.0",
        }
        if component_id != "cyrene-navigator":
            expected["cyrene-runtime-maintenance"] = ">=0.1.0, <0.2.0"
        if dependencies != expected:
            raise ProductReleaseError("Product release dependencies are incomplete or differ from static policy")
    descriptor_path = output / "descriptor.json"
    descriptor_path.write_text(json.dumps(descriptor, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    manifest_path = manifest_dir / manifest_asset_name(descriptor)
    manifest = create_manifest(descriptor_path, manifest_path, artifact_path, payload_root)
    return {
        "manifestPath": str(manifest_path),
        "manifestDigest": manifest["manifestDigest"],
        "artifactPath": str(artifact_path),
        "artifactDigest": manifest["artifact"]["sha256"],
        "version": manifest["version"],
        "innerBundlePath": str(inner_bundle),
        "innerBundleVersion": inner_manifest["version"],
        "sdk": sdk,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service", choices=tuple(SERVICE_COMPONENTS), required=True)
    parser.add_argument("--product-repository", type=Path, required=True)
    parser.add_argument("--product-repository-id", required=True)
    parser.add_argument("--product-commit", required=True)
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--workspace-root", type=Path, required=True)
    parser.add_argument("--workspace-builder-commit", required=True)
    parser.add_argument("--release-tools-root", type=Path, required=True)
    parser.add_argument("--release-tools-commit", required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--catalog-sha256", required=True)
    parser.add_argument("--sdk-fetch-result", type=Path, required=True)
    parser.add_argument("--target-profile", required=True, choices=tuple(NATIVE_PYTHON_TARGETS))
    parser.add_argument("--python-executable", type=Path, required=True)
    parser.add_argument("--uv-executable", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = build_product_service_release(
            service=arguments.service,
            product_repository=arguments.product_repository.resolve(),
            product_repository_id=arguments.product_repository_id,
            product_commit=arguments.product_commit,
            source_ref=arguments.source_ref,
            channel=arguments.channel,
            workspace_root=arguments.workspace_root.resolve(),
            workspace_builder_commit=arguments.workspace_builder_commit,
            release_tools_commit=arguments.release_tools_commit,
            release_tools_root=arguments.release_tools_root.resolve(),
            catalog_path=arguments.catalog.resolve(),
            catalog_sha256=arguments.catalog_sha256,
            sdk_fetch_result=arguments.sdk_fetch_result.resolve(),
            target_profile=arguments.target_profile,
            python_executable=arguments.python_executable,
            uv_executable=arguments.uv_executable,
            output=arguments.output.resolve(),
            run_id=arguments.run_id,
            run_attempt=arguments.run_attempt,
        )
    except (
        ComponentArtifactError,
        ProductReleaseError,
        OSError,
        ValueError,
        KeyError,
        subprocess.SubprocessError,
    ) as error:
        print(f"Product component build failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
