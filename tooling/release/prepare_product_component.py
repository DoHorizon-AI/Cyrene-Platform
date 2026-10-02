"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 prepare_product_component.py                                        │
│  Module: tooling.release.prepare_product_component                     │
│  Role: Build one Product component release from immutable source pins.   │
│                                                                          │
│  模块职责：根据固定 Product、Workspace、Platform 与 SDK 来源准备发布包。│
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tomllib
from pathlib import Path
from typing import Any

from build_product_image_context import ImageContextError, build_context
from build_product_service_release import (
    ProductReleaseError,
    SERVICE_COMPONENTS,
    _git_head,
    _read_json,
    build_product_service_release,
)
from component_descriptor import _catalog_component
from prepare_runtime_sdk_context import prepare as prepare_sdk_context


PLATFORM_REPOSITORY = "DoHorizon-AI/Cyrene-Platform"
PRODUCTS = {
    "catalyst": "DoHorizon-AI/Cyrene-Catalyst",
    "yield": "DoHorizon-AI/Cyrene-Yield",
    "reactor": "DoHorizon-AI/Cyrene-Reactor",
    "exchange": "DoHorizon-AI/Cyrene-Exchange",
    "echo": "DoHorizon-AI/Cyrene-Echo",
    "navigator": "DoHorizon-AI/Cyrene-Navigator",
}
IMAGE_PRODUCTS = {"catalyst", "yield", "reactor", "exchange", "echo"}


class ProductComponentError(ValueError):
    """Raised when Product release preparation cannot preserve all source pins."""


def _component_version(product_root: Path) -> str:
    """Read the Product package version used when no Linux bundle exists."""
    pyproject_path = product_root / "pyproject.toml"
    if pyproject_path.is_symlink() or not pyproject_path.is_file():
        raise ProductComponentError("Product pyproject.toml is missing or unsafe")
    try:
        document = tomllib.loads(pyproject_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ProductComponentError(f"cannot read Product version from pyproject.toml: {error}") from error
    version = document.get("project", {}).get("version")
    if not isinstance(version, str) or not version:
        raise ProductComponentError("Product pyproject.toml has no static project version")
    return version


def _copy_release_outputs(source_root: Path, output_root: Path) -> tuple[Path | None, Path | None]:
    """Collect a Product bundle manifest and payload in the shared output tree."""
    manifests = sorted((source_root / "manifests").glob("*.manifest.json"))
    artifacts = sorted((source_root / "artifacts").glob("*.tar.gz"))
    if len(manifests) != 1 or len(artifacts) != 1:
        raise ProductComponentError("Product bundle builder did not produce one manifest and archive")
    manifest_directory = output_root / "manifests"
    artifact_directory = output_root / "artifacts"
    manifest_directory.mkdir(exist_ok=True)
    artifact_directory.mkdir(exist_ok=True)
    manifest_path = manifest_directory / manifests[0].name
    artifact_path = artifact_directory / artifacts[0].name
    shutil.copyfile(manifests[0], manifest_path)
    shutil.copyfile(artifacts[0], artifact_path)
    return manifest_path, artifact_path


def prepare_product_component(
    *,
    product: str,
    product_root: Path,
    product_repository: str,
    product_commit: str,
    source_ref: str,
    channel: str,
    workspace_root: Path,
    workspace_commit: str,
    release_tools_root: Path,
    release_tools_commit: str,
    catalog_path: Path,
    catalog_sha256: str,
    sdk_fetch_result: Path,
    output: Path,
    run_id: str,
    run_attempt: int,
) -> dict[str, Any]:
    """Build the Product bundle and exact-source OCI context, when supported."""
    if product not in PRODUCTS or product_repository != PRODUCTS[product]:
        raise ProductComponentError("Product name and canonical repository do not match")
    if output.exists() or output.is_symlink():
        raise ProductComponentError(f"release output must not exist: {output}")
    output.mkdir(parents=True)
    component_id = f"cyrene-{product}"
    component, _workflow, catalog = _catalog_component(
        catalog_path,
        catalog_sha256,
        component_id,
        product_repository,
    )
    sdk_result = _read_json(sdk_fetch_result, "verified SDK fetch result")
    sdk_manifest = sdk_result.get("manifest")
    if not isinstance(sdk_manifest, dict):
        raise ProductComponentError("SDK fetch result has no verified manifest")
    sdk_source_commit = sdk_manifest.get("source", {}).get("commit")
    if sdk_source_commit != release_tools_commit:
        raise ProductComponentError("Platform tooling checkout is not the SDK manifest source SHA")
    if _git_head(Path(release_tools_root), PLATFORM_REPOSITORY, "Platform release tooling") != release_tools_commit:
        raise ProductComponentError("Platform release tooling checkout does not match the SDK source SHA")

    python_manifest: Path | None = None
    python_artifact: Path | None = None
    if product in SERVICE_COMPONENTS:
        python_result = build_product_service_release(
            service=product,
            product_repository=product_root,
            product_repository_id=product_repository,
            product_commit=product_commit,
            source_ref=source_ref,
            channel=channel,
            workspace_root=workspace_root,
            workspace_builder_commit=workspace_commit,
            release_tools_commit=release_tools_commit,
            release_tools_root=release_tools_root,
            catalog_path=catalog_path,
            catalog_sha256=catalog_sha256,
            sdk_fetch_result=sdk_fetch_result,
            output=output / "python-build",
            run_id=run_id,
            run_attempt=run_attempt,
        )
        python_manifest, python_artifact = _copy_release_outputs(output / "python-build", output)
        sdk = python_result["sdk"]
        version = python_result["version"]
        if python_manifest is None or python_artifact is None:
            raise ProductComponentError("Product Python release output is missing")
        from component_artifacts import verify_manifest

        manifest = verify_manifest(python_manifest, python_artifact, verify_run=False, verify_attestation=False)
        if manifest.get("componentId") != component_id:
            raise ProductComponentError("Python bundle manifest component identity is incorrect")
    else:
        sdk = prepare_sdk_context(
            sdk_fetch_result,
            channel,
            output / "sdk-build",
        )
        version = _component_version(product_root)

    image_context: dict[str, Any] | None = None
    image_repository = component.get("ociImageRepository")
    if product in IMAGE_PRODUCTS:
        image_context = build_context(
            product=product,
            product_root=product_root,
            product_commit=product_commit,
            workspace_root=workspace_root,
            workspace_commit=workspace_commit,
            output=output / "image-context",
        )
        if not isinstance(image_repository, str) or not image_repository:
            raise ProductComponentError("trusted Product catalog has no OCI image repository")

    lock_path = workspace_root / "release-lock.json"
    lock_sha256 = hashlib.sha256(lock_path.read_bytes()).hexdigest()
    return {
        "componentId": component_id,
        "version": version,
        "pythonBundle": python_artifact is not None,
        "pythonManifest": str(python_manifest) if python_manifest else "",
        "pythonArtifact": str(python_artifact) if python_artifact else "",
        "ociImage": product in IMAGE_PRODUCTS,
        "ociRepository": image_repository or "",
        "imageContext": image_context["context"] if image_context else "",
        "dockerfile": image_context["dockerfile"] if image_context else "",
        "buildInputCommits": image_context["repositories"] if image_context else {},
        "workspaceBuilderCommit": workspace_commit,
        "releaseLockSha256": lock_sha256,
        "sdkBuildContext": sdk["buildContext"],
        "sdkManifestDigest": sdk["manifestDigest"],
        "sdkArtifactDigest": sdk["artifactDigest"],
        "sdkWheelSha256": sdk["wheelSha256"],
        "sdkReleaseId": sdk["releaseId"],
        "sdkSourceCommit": release_tools_commit,
    }


def main() -> int:
    """Build a target-specific Product payload and optional image context."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", choices=tuple(PRODUCTS), required=True)
    parser.add_argument("--product-root", type=Path, required=True)
    parser.add_argument("--product-repository", required=True)
    parser.add_argument("--product-commit", required=True)
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--workspace-root", type=Path, required=True)
    parser.add_argument("--workspace-commit", required=True)
    parser.add_argument("--release-tools-root", type=Path, required=True)
    parser.add_argument("--release-tools-commit", required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--catalog-sha256", required=True)
    parser.add_argument("--sdk-fetch-result", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = prepare_product_component(
            product=arguments.product,
            product_root=arguments.product_root.resolve(),
            product_repository=arguments.product_repository,
            product_commit=arguments.product_commit,
            source_ref=arguments.source_ref,
            channel=arguments.channel,
            workspace_root=arguments.workspace_root.resolve(),
            workspace_commit=arguments.workspace_commit,
            release_tools_root=arguments.release_tools_root.resolve(),
            release_tools_commit=arguments.release_tools_commit,
            catalog_path=arguments.catalog.resolve(),
            catalog_sha256=arguments.catalog_sha256,
            sdk_fetch_result=arguments.sdk_fetch_result.resolve(),
            output=arguments.output.resolve(),
            run_id=arguments.run_id,
            run_attempt=arguments.run_attempt,
        )
    except (
        ImageContextError,
        ProductComponentError,
        ProductReleaseError,
        OSError,
        ValueError,
        KeyError,
    ) as error:
        print(f"Product component preparation failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
