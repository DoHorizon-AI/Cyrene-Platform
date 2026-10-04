#!/usr/bin/env python3
"""Discover and verify one component release through a trusted static catalog."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from component_artifacts import (
    ComponentArtifactError,
    _release_asset_uri,
    _source_repository_parts,
    _verify_attestation,
    _verify_run,
    _verify_release_metadata,
    _verify_index_metadata,
    _validate_index,
    _validate_manifest,
    _read_object,
    manifest_asset_name,
)
from validate_component_schema import validate_document

SOURCE_COMMIT_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")


def _request(url: str, *, api_json: bool = False) -> bytes:
    parsed = urlparse(url)
    allowed_hosts = (
        {"api.github.com"}
        if api_json
        else {
            "github.com",
            "api.github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
        }
    )
    if parsed.scheme != "https" or parsed.hostname not in allowed_hosts:
        raise ComponentArtifactError("download URL is outside the trusted GitHub HTTPS endpoints")
    headers = {"Accept": "application/vnd.github+json"} if api_json else {"Accept": "application/octet-stream"}
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            final = urlparse(response.geturl())
            if final.scheme != "https" or final.hostname not in allowed_hosts:
                raise ComponentArtifactError("GitHub release download redirected outside trusted HTTPS hosts")
            return response.read()
    except (OSError, urllib.error.URLError) as error:
        raise ComponentArtifactError(f"cannot download trusted GitHub release data: {error}") from error


def _api_json(url: str) -> Any:
    try:
        result = json.loads(_request(url, api_json=True).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"GitHub API returned invalid JSON: {error}") from error
    return result


def _catalog_component(
    catalog: dict[str, Any], component_id: str, target_id: str, target: dict[str, Any]
) -> dict[str, Any]:
    components = catalog.get("components")
    component = next(
        (row for row in components or [] if isinstance(row, dict) and row.get("componentId") == component_id),
        None,
    )
    if not isinstance(component, dict):
        raise ComponentArtifactError(f"component is not declared in the trusted catalog: {component_id}")
    target_row = next(
        (row for row in catalog.get("targets", []) if isinstance(row, dict) and row.get("id") == target_id),
        None,
    )
    if not isinstance(target_row, dict) or target_row.get("target") != target:
        raise ComponentArtifactError("requested target ID and complete target do not match the trusted catalog")
    build_dependency = component.get("role") == "build-dependency"
    declarations = [
        row
        for row in component.get("targets", [])
        if isinstance(row, dict)
        and row.get("targetId") == target_id
        and row.get("artifactKind") in component.get("artifactKinds", [])
    ]
    permitted = [
        row
        for row in declarations
        if row.get("support") in (
            {"supported", "contract-only"}
            if build_dependency or row.get("artifactKind") == "oci-image"
            else {"supported"}
        )
        and target_row.get("hostSupport")
        in (
            {"supported", "contract-only"}
            if build_dependency or row.get("artifactKind") == "oci-image"
            else {"supported"}
        )
    ]
    if not permitted:
        raise ComponentArtifactError("requested component target is not authorized by the trusted catalog")
    return component


def _channel_refs(catalog: dict[str, Any], channel: str) -> set[str]:
    policy = catalog.get("channels", {}).get(channel)
    refs = policy.get("sourceRefs") if isinstance(policy, dict) else None
    if not isinstance(refs, list) or not refs or not all(isinstance(row, str) for row in refs):
        raise ComponentArtifactError(f"trusted catalog has no valid source refs for {channel}")
    return set(refs)


def _download_release_index(
    catalog_path: Path,
    catalog: dict[str, Any],
    publisher: str,
    channel: str,
    destination: Path,
    expected_source_commit: str | None = None,
) -> tuple[dict[str, Any], dict[str, Any], str]:
    channels = catalog.get("channels", {})
    policy = channels.get(channel) if isinstance(channels, dict) else None
    prerelease = policy.get("releasePrerelease") if isinstance(policy, dict) else None
    if not isinstance(prerelease, bool):
        raise ComponentArtifactError(f"trusted catalog has no release policy for {channel}")
    if expected_source_commit is not None and not SOURCE_COMMIT_RE.fullmatch(expected_source_commit):
        raise ComponentArtifactError("expected source commit must be a full lowercase Git SHA")
    expected_release_id = (
        f"{channel}-{expected_source_commit}" if expected_source_commit is not None else None
    )
    publisher_rows = catalog.get("publishers")
    if not isinstance(publisher_rows, list):
        raise ComponentArtifactError("trusted catalog has no publisher list")
    candidates: list[tuple[str, dict[str, Any]]] = []
    for publisher_row in publisher_rows:
        if not isinstance(publisher_row, dict) or publisher_row.get("repository") != publisher:
            continue
        discovery = publisher_row.get("releaseDiscovery")
        api_uri = discovery.get("apiUri") if isinstance(discovery, dict) else None
        if not isinstance(api_uri, str):
            continue
        parsed = urlparse(api_uri)
        owner_repo = parsed.path.split("/repos/", 1)[-1].split("/releases", 1)[0]
        if not owner_repo or publisher != owner_repo:
            continue
        if discovery.get("indexAssetName") != "component-release-index-v1.json":
            continue
        if expected_release_id is not None:
            canonical_api_uri = f"https://api.github.com/repos/{publisher}/releases?per_page=100"
            if api_uri != canonical_api_uri:
                raise ComponentArtifactError("trusted release discovery URI is not canonical for exact release lookup")
            exact_release_uri = (
                f"https://api.github.com/repos/{publisher}/releases/tags/{expected_release_id}"
            )
            release = _api_json(exact_release_uri)
            if not isinstance(release, dict):
                raise ComponentArtifactError("GitHub exact release lookup must return one release object")
            releases = [release]
        else:
            releases = _api_json(api_uri)
            if not isinstance(releases, list):
                raise ComponentArtifactError("GitHub Releases API must return a release array")
        for release in releases:
            if not isinstance(release, dict):
                continue
            tag = release.get("tag_name")
            if (
                release.get("draft") is False
                and release.get("immutable") is True
                and release.get("prerelease") is prerelease
                and isinstance(tag, str)
                and tag.startswith(f"{channel}-")
                and (expected_release_id is None or tag == expected_release_id)
            ):
                candidates.append((api_uri, release))
    candidates.sort(
        key=lambda item: str(item[1].get("published_at") or item[1].get("created_at") or ""),
        reverse=True,
    )
    refs = _channel_refs(catalog, channel)
    for api_uri, release in candidates:
        tag = release["tag_name"]
        repo_url = publisher
        owner, repository = repo_url.split("/", 1)
        api_owner_repo = urlparse(api_uri).path.split("/repos/", 1)[-1].split("/releases", 1)[0]
        if api_owner_repo != repo_url:
            continue
        asset = next(
            (
                row
                for row in release.get("assets", [])
                if isinstance(row, dict) and row.get("name") == "component-release-index-v1.json"
            ),
            None,
        )
        if not isinstance(asset, dict):
            continue
        expected_uri = (
            f"https://github.com/{owner}/{repository}/releases/download/{tag}/component-release-index-v1.json"
        )
        if asset.get("browser_download_url") != expected_uri:
            continue
        raw = _request(expected_uri)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(raw)
        index = _read_object(destination)
        schema_errors = validate_document(destination, catalog_path.parent, "index")
        if schema_errors:
            raise ComponentArtifactError(
                "component index does not match its pinned schema: " + "; ".join(schema_errors)
            )
        if index.get("repository") != repo_url or index.get("channel") != channel:
            continue
        source = index.get("source", {})
        if source.get("ref") not in refs or tag != f"{channel}-{source.get('commit')}":
            continue
        if expected_source_commit is not None and source.get("commit") != expected_source_commit:
            continue
        errors = _validate_index(index)
        if errors:
            raise ComponentArtifactError("invalid component index: " + "; ".join(errors))
        _verify_index_metadata(index, catalog)
        _verify_run(index["source"], index["provenance"]["attestation"])
        _verify_attestation(
            str(destination),
            index["source"],
            index["provenance"]["attestation"],
            subject_name="component-release-index-v1.json",
        )
        return index, release, expected_uri
    raise ComponentArtifactError(f"no trusted immutable {channel} component release index was found")


def fetch_component(
    catalog_path: Path,
    component_id: str,
    target_id: str,
    channel: str,
    output: Path,
    expected_source_commit: str | None = None,
) -> dict[str, Any]:
    catalog = _read_object(catalog_path)
    target_row = next(
        (row for row in catalog.get("targets", []) if isinstance(row, dict) and row.get("id") == target_id),
        None,
    )
    if not isinstance(target_row, dict) or not isinstance(target_row.get("target"), dict):
        raise ComponentArtifactError(f"target ID is absent from the trusted catalog: {target_id}")
    component = _catalog_component(catalog, component_id, target_id, target_row["target"])
    publisher = component.get("publisher")
    if not isinstance(publisher, str):
        raise ComponentArtifactError("trusted component has no canonical publisher")
    index, _release, _index_uri = _download_release_index(
        catalog_path,
        catalog,
        publisher,
        channel,
        output / "component-release-index-v1.json",
        expected_source_commit,
    )
    if expected_source_commit is not None and index.get("source", {}).get("commit") != expected_source_commit:
        raise ComponentArtifactError("component release index source commit differs from the requested exact commit")
    publisher_row = next(
        (row for row in catalog.get("publishers", []) if isinstance(row, dict) and row.get("repository") == publisher),
        None,
    )
    if not isinstance(publisher_row, dict):
        raise ComponentArtifactError("component publisher is not declared by the trusted catalog")
    source_owner, source_repository = _source_repository_parts(index["source"]["repository"])
    if f"{source_owner}/{source_repository}" != publisher:
        raise ComponentArtifactError("trusted index publisher does not match component publisher")
    release_row = next(
        (
            row
            for row in index["releases"]
            if row.get("componentId") == component_id and row.get("target") == target_row["target"]
        ),
        None,
    )
    if not isinstance(release_row, dict):
        raise ComponentArtifactError(f"no {channel} artifact exists for {component_id}/{target_id}")
    manifest_uri = release_row["manifestUri"]
    expected_manifest_uri = _release_asset_uri(
        {
            "source": {"repository": f"https://github.com/{publisher}"},
            "releaseId": f"{channel}-{index['source']['commit']}",
        },
        manifest_uri.rsplit("/", 1)[-1],
    )
    if manifest_uri != expected_manifest_uri:
        raise ComponentArtifactError("index manifestUri is not the expected immutable release asset URL")
    manifest_path = output / manifest_uri.rsplit("/", 1)[-1]
    manifest_path.write_bytes(_request(manifest_uri))
    manifest = _read_object(manifest_path)
    schema_errors = validate_document(manifest_path, catalog_path.parent, "manifest")
    if schema_errors:
        raise ComponentArtifactError("component manifest does not match its pinned schema: " + "; ".join(schema_errors))
    if manifest.get("manifestDigest") != release_row.get("manifestDigest"):
        raise ComponentArtifactError("downloaded manifest digest does not match the trusted index")
    errors = _validate_manifest(manifest)
    if errors:
        raise ComponentArtifactError("invalid component manifest: " + "; ".join(errors))
    if manifest["componentId"] != component_id or manifest["target"] != target_row["target"]:
        raise ComponentArtifactError("downloaded manifest identity does not match requested component target")
    if manifest["source"]["repository"] != f"https://github.com/{publisher}":
        raise ComponentArtifactError("manifest source repository does not match trusted publisher")
    if expected_source_commit is not None and manifest["source"].get("commit") != expected_source_commit:
        raise ComponentArtifactError("component manifest source commit differs from the requested exact commit")
    _verify_release_metadata(manifest, catalog, manifest_path)
    _verify_run(manifest["source"], manifest["provenance"]["attestation"])
    artifact = manifest["artifact"]
    artifact_path: Path | None = None
    if artifact["kind"] in {"native-binary", "python-bundle"}:
        artifact_uri = artifact["uri"]
        expected_artifact_uri = _release_asset_uri(manifest, artifact_uri.rsplit("/", 1)[-1])
        if artifact_uri != expected_artifact_uri:
            raise ComponentArtifactError("artifact URI is not the expected immutable release asset URL")
        artifact_path = output / artifact_uri.rsplit("/", 1)[-1]
        artifact_path.write_bytes(_request(artifact_uri))
        if artifact_path.stat().st_size != artifact["sizeBytes"]:
            raise ComponentArtifactError("downloaded payload size does not match manifest")
        from component_artifacts import _archive_files, _sha256_file

        if _sha256_file(artifact_path) != artifact["sha256"] or _archive_files(artifact_path) != artifact["files"]:
            raise ComponentArtifactError("downloaded payload bytes do not match manifest digest/file map")
        _verify_attestation(
            str(artifact_path),
            manifest["source"],
            manifest["provenance"]["attestation"],
            subject_name=manifest["provenance"]["attestation"]["subjectName"],
        )
    else:
        _verify_attestation(
            f"oci://{artifact['repository']}@{artifact['digest']}",
            manifest["source"],
            manifest["provenance"]["attestation"],
            subject_name=artifact["repository"],
        )
    return {
        "index": index,
        "manifest": manifest,
        "manifestPath": str(manifest_path),
        "artifactPath": str(artifact_path) if artifact_path else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, required=True, help="Workspace catalog from a trusted immutable commit")
    parser.add_argument("--component", required=True)
    parser.add_argument("--target-id", required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument(
        "--source-commit",
        help="fetch only the immutable release for this full lowercase source SHA; never fall back to channel latest",
    )
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = fetch_component(
            args.catalog,
            args.component,
            args.target_id,
            args.channel,
            args.output.resolve(),
            expected_source_commit=args.source_commit,
        )
    except (ComponentArtifactError, OSError, ValueError, KeyError) as error:
        print(f"fetch component failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
