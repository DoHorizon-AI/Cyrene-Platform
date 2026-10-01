#!/usr/bin/env python3
"""Write a component release descriptor from explicit CI inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

from component_artifacts import _component_target


def _json_value(raw: str, label: str) -> Any:
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"{label} must be valid JSON: {error}") from error


def _catalog_component(
    catalog_path: Path,
    expected_sha256: str,
    component_id: str,
    repository: str,
) -> tuple[dict[str, Any], str, dict[str, Any]]:
    """Read a component and publisher only from the pinned static catalog."""
    raw = catalog_path.read_bytes()
    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != expected_sha256:
        raise ValueError("static component catalog bytes do not match the pinned SHA-256")
    try:
        catalog = json.loads(
            raw.decode("utf-8", "strict"),
            object_pairs_hook=lambda pairs: _unique_object(pairs),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"static component catalog is invalid JSON: {error}") from error
    if not isinstance(catalog, dict) or catalog.get("schemaVersion") != 1:
        raise ValueError("static component catalog has an unsupported schemaVersion")
    owner_repository, repository_name = repository.split("/", 1)
    publisher_id = f"{owner_repository}/{repository_name}"
    publishers = catalog.get("publishers")
    components = catalog.get("components")
    if not isinstance(publishers, list) or not isinstance(components, list):
        raise ValueError("static component catalog publishers/components must be arrays")
    publisher = next(
        (entry for entry in publishers if isinstance(entry, dict) and entry.get("repository") == publisher_id),
        None,
    )
    workflow = f"{publisher_id}/.github/workflows/component-release.yml"
    if not isinstance(publisher, dict) or publisher.get("workflow") != workflow:
        raise ValueError("source repository or workflow is not authorized by the static component catalog")
    component = next(
        (entry for entry in components if isinstance(entry, dict) and entry.get("componentId") == component_id),
        None,
    )
    if not isinstance(component, dict) or component.get("publisher") != publisher_id:
        raise ValueError("component is missing from the static catalog or has a different publisher")
    dependencies = component.get("dependencies")
    if not isinstance(dependencies, list):
        raise ValueError("catalog component dependencies must be an array")
    copied_dependencies: list[dict[str, str]] = []
    seen: set[str] = set()
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            raise ValueError("catalog dependency must be an object")
        dependency_id = dependency.get("componentId")
        version_range = dependency.get("versionRange")
        if (
            not isinstance(dependency_id, str)
            or not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", dependency_id)
            or dependency_id in seen
            or (
                version_range is not None
                and (not isinstance(version_range, str) or not version_range.strip() or len(version_range) > 128)
            )
        ):
            raise ValueError("catalog dependency has a missing, duplicate, or malformed component/version range")
        seen.add(dependency_id)
        copied = {"componentId": dependency_id}
        if version_range is not None:
            copied["versionRange"] = version_range
        copied_dependencies.append(copied)
    copied_dependencies.sort(key=lambda entry: (entry["componentId"], entry.get("versionRange", "")))
    catalog_component_id = component.get("componentId")
    if isinstance(catalog_component_id, str):
        required_dependencies = {
            "cyrene-catalyst": {
                "cyrene-runtime-maintenance-sdk": "=0.1.0",
                "cyrene-runtime-maintenance": ">=0.1.0, <0.2.0",
            },
            "cyrene-yield": {
                "cyrene-runtime-maintenance-sdk": "=0.1.0",
                "cyrene-runtime-maintenance": ">=0.1.0, <0.2.0",
            },
            "cyrene-reactor": {
                "cyrene-runtime-maintenance-sdk": "=0.1.0",
                "cyrene-runtime-maintenance": ">=0.1.0, <0.2.0",
            },
            "cyrene-exchange": {
                "cyrene-runtime-maintenance-sdk": "=0.1.0",
                "cyrene-runtime-maintenance": ">=0.1.0, <0.2.0",
            },
            "cyrene-echo": {
                "cyrene-runtime-maintenance-sdk": "=0.1.0",
                "cyrene-runtime-maintenance": ">=0.1.0, <0.2.0",
            },
            "cyrene-navigator": {"cyrene-runtime-maintenance-sdk": "=0.1.0"},
        }.get(catalog_component_id)
        if required_dependencies is not None:
            actual_dependencies = {entry["componentId"]: entry.get("versionRange") for entry in copied_dependencies}
            if actual_dependencies != required_dependencies:
                raise ValueError("Product component dependencies do not match the frozen SDK/broker requirements")
    restart = component.get("restart")
    if not isinstance(restart, dict):
        raise ValueError("catalog component restart must be an object")
    component = dict(component)
    component["dependencies"] = copied_dependencies
    component["restart"] = restart
    return component, workflow, catalog


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--component-id", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--repository", required=True, help="GitHub owner/repository")
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", type=int, required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--catalog-sha256", required=True, help="pinned raw catalog SHA-256, lowercase hex")
    parser.add_argument("--target-json", required=True)
    parser.add_argument("--artifact-kind", choices=("native-binary", "python-bundle", "oci-image"), required=True)
    parser.add_argument("--subject-name", required=True)
    parser.add_argument("--format", choices=("tar.gz", "tar.zst", "zip"))
    parser.add_argument("--entrypoint")
    parser.add_argument("--oci-repository")
    parser.add_argument("--oci-digest")
    parser.add_argument("--oci-platform-json")
    parser.add_argument("--compatibility-json")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
            raise ValueError("repository must be owner/repository")
        if not re.fullmatch(r"[0-9a-f]{40}([0-9a-f]{24})?", args.source_commit):
            raise ValueError("source commit must be a full lower-case Git SHA")
        if not args.run_id.isdigit() or args.run_attempt < 1:
            raise ValueError("GitHub Actions run identity is invalid")
        if not re.fullmatch(r"[0-9a-f]{64}", args.catalog_sha256):
            raise ValueError("catalog-sha256 must be 64 lowercase hexadecimal characters")
        catalog_component, workflow, catalog = _catalog_component(
            args.catalog, args.catalog_sha256, args.component_id, args.repository
        )
        target = _json_value(args.target_json, "target-json")
        if not isinstance(target, dict):
            raise ValueError("target has an invalid JSON shape")
        source_url = f"https://github.com/{args.repository}"
        run_url = f"{source_url}/actions/runs/{args.run_id}/attempts/{args.run_attempt}"
        descriptor: dict[str, Any] = {
            "schemaVersion": 1,
            "releaseId": f"{args.channel}-{args.source_commit}",
            "componentId": args.component_id,
            "version": args.version,
            "channel": args.channel,
            "target": target,
            "artifact": {"kind": args.artifact_kind},
            "dependencies": catalog_component["dependencies"],
            "restart": catalog_component["restart"],
            "source": {"repository": source_url, "ref": args.source_ref, "commit": args.source_commit},
            "provenance": {
                "attestation": {
                    "kind": "github-artifact-attestation",
                    "subjectName": args.subject_name,
                    "repository": args.repository,
                    "workflow": workflow,
                    "predicateType": "https://slsa.dev/provenance/v1",
                    "run": {"id": args.run_id, "attempt": args.run_attempt, "url": run_url},
                }
            },
        }
        artifact = descriptor["artifact"]
        if args.artifact_kind == "oci-image":
            if not args.oci_repository or not args.oci_digest or not args.oci_platform_json:
                raise ValueError("OCI manifests require repository, digest, and platform")
            artifact.update(
                {
                    "repository": args.oci_repository,
                    "digest": args.oci_digest,
                    "platform": _json_value(args.oci_platform_json, "oci-platform-json"),
                }
            )
        else:
            if not args.format:
                raise ValueError("file manifests require --format")
            artifact["format"] = args.format
            if args.artifact_kind == "native-binary":
                if not args.entrypoint:
                    raise ValueError("native binary manifests require --entrypoint")
                artifact["entrypoint"] = args.entrypoint
        _component_target(catalog, args.component_id, artifact, target)
        if args.compatibility_json:
            compatibility = _json_value(args.compatibility_json, "compatibility-json")
            if not isinstance(compatibility, dict):
                raise ValueError("compatibility-json must be a JSON object")
            descriptor["compatibility"] = compatibility
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(descriptor, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"component descriptor failed: {error}", file=sys.stderr)
        return 2
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
