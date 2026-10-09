"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 component_artifacts.py                                               │
│  Module: tooling.release.component_artifacts                            │
│  Role: Package and verify immutable component release metadata.         │
│                                                                          │
│  模块职责：打包并校验不可变组件发布制品、manifest 与 release index。      │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import gzip
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request
from pathlib import Path, PurePosixPath
from typing import Any, Callable
from urllib.parse import quote, urlparse

import rfc8785

SCHEMA_VERSION = 1
COMPONENT_ID_RE = re.compile(r"^[a-z][a-z0-9-]{0,63}$")
SHA256_RE = re.compile(r"^sha256:([0-9a-f]{64})$")
SOURCE_COMMIT_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
RUN_PATH_RE = re.compile(r"^https://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/actions/runs/(\d+)/attempts/(\d+)$")
ATTACHMENT_KINDS = {"native-binary", "python-bundle"}
ARTIFACT_KINDS = ATTACHMENT_KINDS | {"oci-image"}
ATTESTATION_PREDICATE = "https://slsa.dev/provenance/v1"
SEMVER_RANGE_TERM = re.compile(r"^(?:=|>=|>|<=|<|\^|~)?[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$")
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
RAW_NATIVE_MANIFEST_COMPONENTS = frozenset({"cy-package-runtime", "cyrene-kernel", "cyrene-runtime-maintenance"})
RAW_NATIVE_MANIFEST_TARGETS = {
    "22.04": {
        "os": "linux",
        "osVersion": "22.04",
        "distribution": "ubuntu",
        "distributionVersion": "22.04",
        "architecture": "x86_64",
        "abi": "glibc-2.35",
        "runtime": "systemd",
    },
    "24.04": {
        "os": "linux",
        "osVersion": "24.04",
        "distribution": "ubuntu",
        "distributionVersion": "24.04",
        "architecture": "x86_64",
        "abi": "glibc-2.39",
        "runtime": "systemd",
    },
}


class ComponentArtifactError(ValueError):
    """Raised when a component payload or release manifest is invalid."""


def _canonical_json(value: Any) -> bytes:
    """Serialize JSON with the RFC 8785 implementation used by all consumers.

    The public digest rules use JCS, not a Python-specific sorted JSON variant.
    Non-finite values, unsupported numbers, and unsafe integers are rejected by
    the rfc8785 package before they can enter a manifest or index.
    """

    def reject_non_safe_numbers(item: Any) -> None:
        if isinstance(item, float):
            raise ComponentArtifactError("RFC 8785 release data only permits safe integer numbers")
        if isinstance(item, int) and not isinstance(item, bool) and abs(item) > 9_007_199_254_740_991:
            raise ComponentArtifactError("RFC 8785 release data contains an unsafe integer")
        if isinstance(item, dict):
            for key, nested in item.items():
                if not isinstance(key, str):
                    raise ComponentArtifactError("RFC 8785 object keys must be strings")
                reject_non_safe_numbers(nested)
        elif isinstance(item, (list, tuple)):
            for nested in item:
                reject_non_safe_numbers(nested)

    reject_non_safe_numbers(value)
    try:
        return rfc8785.dumps(value)
    except (TypeError, ValueError, OverflowError) as error:
        raise ComponentArtifactError(f"value is not RFC 8785 canonicalizable: {error}") from error


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return "sha256:" + digest.hexdigest()


def _safe_relative(path: str) -> PurePosixPath:
    relative = PurePosixPath(path)
    if (
        not path
        or "\\" in path
        or relative.is_absolute()
        or relative.as_posix() != path
        or any(part in {"", ".", ".."} for part in relative.parts)
    ):
        raise ComponentArtifactError(f"unsafe payload path: {path!r}")
    return relative


def _payload_files(root: Path) -> dict[str, str]:
    """Hash regular payload files and reject symlinks or special files."""
    payload_root = Path(root)
    if payload_root.is_symlink() or not payload_root.is_dir():
        raise ComponentArtifactError(f"payload root is missing or unsafe: {payload_root}")
    files: dict[str, str] = {}
    for current, directory_names, file_names in os.walk(payload_root, followlinks=False):
        current_path = Path(current)
        for name in directory_names:
            path = current_path / name
            if path.is_symlink() or not path.is_dir():
                raise ComponentArtifactError(f"payload contains an unsafe directory: {path}")
        for name in file_names:
            path = current_path / name
            relative = path.relative_to(payload_root).as_posix()
            _safe_relative(relative)
            info = path.lstat()
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
                raise ComponentArtifactError(f"payload contains a non-regular file: {relative}")
            files[relative] = _sha256_file(path)
    if not files:
        raise ComponentArtifactError("payload is empty")
    return dict(sorted(files.items()))


def _write_deterministic_tar_gz(payload_root: Path, output_path: Path) -> None:
    """Create a timestamp-free tarball with normalized ownership and modes."""
    files = _payload_files(payload_root)
    destination = Path(output_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("wb") as raw_stream:
        with gzip.GzipFile(filename="", fileobj=raw_stream, mode="wb", mtime=0, compresslevel=9) as gzip_stream:
            with tarfile.open(fileobj=gzip_stream, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for relative in files:
                    source = Path(payload_root) / relative
                    info = tarfile.TarInfo(name=relative)
                    source_info = source.stat()
                    info.size = source_info.st_size
                    info.mtime = 0
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mode = 0o755 if source_info.st_mode & 0o111 else 0o644
                    with source.open("rb") as stream:
                        archive.addfile(info, stream)


def _archive_files(path: Path) -> dict[str, str]:
    """Hash tar.gz regular files without extracting untrusted paths."""
    result: dict[str, str] = {}
    try:
        with tarfile.open(path, mode="r:gz") as archive:
            for member in archive.getmembers():
                relative = _safe_relative(member.name)
                if not member.isfile() or relative.as_posix() in result:
                    raise ComponentArtifactError(f"archive contains a duplicate or unsupported entry: {member.name}")
                stream = archive.extractfile(member)
                if stream is None:
                    raise ComponentArtifactError(f"archive entry has no content: {member.name}")
                digest = hashlib.sha256()
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
                result[relative.as_posix()] = "sha256:" + digest.hexdigest()
    except (OSError, tarfile.TarError) as error:
        raise ComponentArtifactError(f"cannot read component archive {path}: {error}") from error
    if not result:
        raise ComponentArtifactError("component archive is empty")
    return dict(sorted(result.items()))


def _manifest_digest(document: dict[str, Any]) -> str:
    """Return the JCS SHA-256 digest after omitting the digest field itself."""
    digest_input = {key: value for key, value in document.items() if key != "manifestDigest"}
    return "sha256:" + hashlib.sha256(_canonical_json(digest_input)).hexdigest()


def _index_digest(document: dict[str, Any]) -> str:
    """Return the JCS SHA-256 digest after omitting the digest field itself."""
    digest_input = {key: value for key, value in document.items() if key != "indexDigest"}
    return "sha256:" + hashlib.sha256(_canonical_json(digest_input)).hexdigest()


def _source_repository_parts(value: str) -> tuple[str, str]:
    parsed = urlparse(value)
    if parsed.scheme != "https" or parsed.hostname != "github.com":
        raise ComponentArtifactError("source.repository must be a canonical GitHub HTTPS URL")
    parts = [part for part in parsed.path.split("/") if part]
    if len(parts) != 2 or parsed.query or parsed.fragment:
        raise ComponentArtifactError("source.repository must identify exactly one GitHub repository")
    return parts[0], parts[1]


def _target_slug(target: dict[str, Any]) -> str:
    """Create a stable, path-safe name for a single target."""
    values = [target["os"]]
    if target.get("distribution"):
        values.append(target["distribution"])
    if target.get("distributionVersion"):
        values.append(target["distributionVersion"])
    values.append(target["architecture"])
    if target.get("abi"):
        values.append(target["abi"])
    if target.get("runtime"):
        values.append(target["runtime"])
    normalized = "-".join(values).lower().replace("_", "-").replace(":", "-").replace(".", "-")
    slug = re.sub(r"[^a-z0-9-]+", "-", normalized).strip("-")
    if not slug or len(slug) > 120:
        raise ComponentArtifactError("target cannot be represented as a stable asset name")
    return slug


def manifest_asset_name(manifest: dict[str, Any]) -> str:
    """Return the canonical asset name for a target-specific manifest."""
    return f"{manifest['componentId']}-{_target_slug(manifest['target'])}.manifest.json"


def _release_asset_uri(document: dict[str, Any], asset_name: str) -> str:
    owner, repository = _source_repository_parts(document["source"]["repository"])
    release_id = quote(document["releaseId"], safe="-")
    return f"https://github.com/{owner}/{repository}/releases/download/{release_id}/{quote(asset_name, safe='.-_')}"


def _validate_target(target: Any) -> list[str]:
    errors: list[str] = []
    if not isinstance(target, dict):
        return ["target must be an object"]
    if target.get("os") not in {"linux", "windows"}:
        errors.append("target.os must be linux or windows")
    for field in ("osVersion", "architecture"):
        if not isinstance(target.get(field), str) or not target[field]:
            errors.append(f"target.{field} must be a non-empty string")
    if target.get("architecture") not in {"x86_64", "aarch64"}:
        errors.append("target.architecture must be x86_64 or aarch64")
    return errors


def _validate_provenance(source: dict[str, Any], provenance: Any) -> list[str]:
    errors: list[str] = []
    if not isinstance(provenance, dict):
        return ["provenance must be an object"]
    attestation = provenance.get("attestation")
    if not isinstance(attestation, dict):
        return ["provenance.attestation must be an object"]
    for key in ("kind", "subjectName", "repository", "workflow", "predicateType", "run"):
        if key not in attestation:
            errors.append(f"provenance.attestation.{key} is required")
    if attestation.get("kind") != "github-artifact-attestation":
        errors.append("provenance.attestation.kind must be github-artifact-attestation")
    if attestation.get("predicateType") != ATTESTATION_PREDICATE:
        errors.append(f"provenance.attestation.predicateType must be {ATTESTATION_PREDICATE}")
    if "uri" in attestation and (
        not isinstance(attestation["uri"], str) or not attestation["uri"].startswith("https://github.com/")
    ):
        errors.append("provenance.attestation.uri must be a GitHub HTTPS URI")
    try:
        owner, repository_name = _source_repository_parts(source.get("repository", ""))
        repository = f"{owner}/{repository_name}"
        workflow = attestation.get("workflow")
        if attestation.get("repository") != repository:
            errors.append("attestation.repository must match source.repository")
        if not isinstance(workflow, str) or not workflow.startswith(f"{repository}/.github/workflows/"):
            errors.append("attestation.workflow must identify a workflow in the source repository")
    except ComponentArtifactError as error:
        errors.append(str(error))
    run = attestation.get("run")
    if not isinstance(run, dict):
        errors.append("provenance.attestation.run must be an object")
    else:
        if not isinstance(run.get("id"), str) or not run["id"].isdigit():
            errors.append("provenance.attestation.run.id must be a numeric string")
        if not isinstance(run.get("attempt"), int) or run["attempt"] < 1:
            errors.append("provenance.attestation.run.attempt must be a positive integer")
        if not isinstance(run.get("url"), str) or not RUN_PATH_RE.fullmatch(run["url"]):
            errors.append("provenance.attestation.run.url must be the GitHub Actions attempt URL")
    return errors


def _validate_compatibility(value: Any) -> list[str]:
    """Validate a Platform contract pin shared by a coordinated component group."""
    if value is None:
        return []
    if not isinstance(value, dict):
        return ["compatibility must be an object"]
    errors: list[str] = []
    for key in ("groupId", "contractApiVersion", "wireApiVersion"):
        if not isinstance(value.get(key), str) or not value[key]:
            errors.append(f"compatibility.{key} must be a non-empty string")
    if "groupVersion" in value and (not isinstance(value["groupVersion"], str) or not value["groupVersion"]):
        errors.append("compatibility.groupVersion must be a non-empty string")
    lock = value.get("contractLock")
    if not isinstance(lock, dict):
        errors.append("compatibility.contractLock must be an object")
    else:
        for key in ("repository", "path", "commit", "sha256"):
            if not isinstance(lock.get(key), str) or not lock[key]:
                errors.append(f"compatibility.contractLock.{key} must be a non-empty string")
        if isinstance(lock.get("repository"), str) and not re.fullmatch(
            r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", lock["repository"]
        ):
            errors.append("compatibility.contractLock.repository must be owner/repository")
        if isinstance(lock.get("commit"), str) and not SOURCE_COMMIT_RE.fullmatch(lock["commit"]):
            errors.append("compatibility.contractLock.commit must be a full lower-case Git SHA")
        if isinstance(lock.get("path"), str):
            try:
                _safe_relative(lock["path"])
            except ComponentArtifactError as error:
                errors.append(str(error))
        if isinstance(lock.get("sha256"), str) and not SHA256_RE.fullmatch(lock["sha256"]):
            errors.append("compatibility.contractLock.sha256 must be sha256:<64 lowercase hex>")
    return errors


def _dependency_rows(value: Any, label: str, *, strict: bool = False) -> list[tuple[str, str | None]]:
    """Validate and normalize catalog or manifest dependency rows."""
    if not isinstance(value, list):
        raise ComponentArtifactError(f"{label} must be an array")
    normalized: list[tuple[str, str | None]] = []
    seen: set[str] = set()
    for row in value:
        if not isinstance(row, dict) or set(row) - {"componentId", "versionRange"}:
            raise ComponentArtifactError(f"{label} entries must contain only componentId/versionRange")
        component_id = row.get("componentId")
        version_range = row.get("versionRange")
        if not isinstance(component_id, str) or not COMPONENT_ID_RE.fullmatch(component_id):
            raise ComponentArtifactError(f"{label} entry has an invalid componentId")
        if component_id in seen:
            raise ComponentArtifactError(f"{label} contains duplicate component IDs")
        seen.add(component_id)
        if version_range is not None and (
            not isinstance(version_range, str) or not version_range.strip() or len(version_range) > 128
        ):
            raise ComponentArtifactError(f"{label} entry has an invalid versionRange")
        if version_range is not None and any(
            not SEMVER_RANGE_TERM.fullmatch(term.strip()) for term in version_range.split(",")
        ):
            raise ComponentArtifactError(f"{label} entry has a malformed SemVer versionRange")
        if strict and version_range is None:
            raise ComponentArtifactError(f"{label} v2 entry must pin versionRange")
        normalized.append((component_id, version_range))
    return sorted(normalized, key=lambda row: (row[0], row[1] or ""))


def _validate_manifest(document: dict[str, Any]) -> list[str]:
    """Return structural and canonical-digest errors for a release manifest."""
    errors: list[str] = []
    required = {
        "schemaVersion",
        "releaseId",
        "componentId",
        "version",
        "channel",
        "target",
        "artifact",
        "dependencies",
        "restart",
        "source",
        "provenance",
        "manifestDigest",
    }
    missing = sorted(required - set(document))
    if missing:
        errors.append("missing manifest keys: " + ", ".join(missing))
    schema_version = document.get("schemaVersion")
    if schema_version not in {1, 2}:
        errors.append("schemaVersion must be 1 or 2")
    if schema_version == 2:
        required.update({"protocolVersion", "contentDigest"})
        missing_v2 = sorted(required - set(document))
        if missing_v2:
            errors.append("missing v2 manifest keys: " + ", ".join(missing_v2))
        protocol_version = document.get("protocolVersion")
        if not isinstance(protocol_version, str) or not re.fullmatch(r"[a-z][a-z0-9._-]{0,127}", protocol_version):
            errors.append("protocolVersion must be a valid wire/API identifier")
    component_id = document.get("componentId")
    if not isinstance(component_id, str) or not COMPONENT_ID_RE.fullmatch(component_id):
        errors.append("componentId must use lower-case hyphen-separated identifiers")
    for key in ("releaseId", "version"):
        if not isinstance(document.get(key), str) or not document[key]:
            errors.append(f"{key} must be a non-empty string")
    if document.get("channel") not in {"stable", "preview"}:
        errors.append("channel must be stable or preview")
    errors.extend(_validate_target(document.get("target")))

    source = document.get("source")
    if not isinstance(source, dict):
        errors.append("source must be an object")
    else:
        try:
            _source_repository_parts(source.get("repository", ""))
        except ComponentArtifactError as error:
            errors.append(str(error))
        commit = source.get("commit")
        if not isinstance(commit, str) or not SOURCE_COMMIT_RE.fullmatch(commit):
            errors.append("source.commit must be a full lower-case Git SHA")
        ref = source.get("ref")
        if not isinstance(ref, str) or not re.fullmatch(r"refs/(heads|tags)/[A-Za-z0-9._/-]+", ref):
            errors.append("source.ref must identify a Git branch or tag")
        if schema_version == 2:
            if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
                errors.append("v2 source.commit must be a full 40-character lowercase Git SHA")
            if document.get("releaseId") != f"{document.get('channel')}-{commit}":
                errors.append("v2 releaseId must equal channel plus exact source.commit")
            allowed_refs = {
                "preview": {"refs/heads/develop"},
                "stable": {"refs/heads/main", "refs/heads/release"},
            }
            if ref not in allowed_refs.get(document.get("channel"), set()):
                errors.append("v2 source.ref does not match the release channel")
        errors.extend(_validate_provenance(source, document.get("provenance")))

    artifact = document.get("artifact")
    if not isinstance(artifact, dict) or artifact.get("kind") not in ARTIFACT_KINDS:
        errors.append("artifact.kind must be native-binary, python-bundle, or oci-image")
    elif artifact["kind"] in ATTACHMENT_KINDS:
        if artifact.get("format") not in {"tar.gz", "tar.zst", "zip"}:
            errors.append("file artifact format is unsupported")
        digest = artifact.get("sha256")
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append("artifact.sha256 must be sha256:<64 lowercase hex>")
        size = artifact.get("sizeBytes")
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
            errors.append("artifact.sizeBytes must be a positive integer")
        files = artifact.get("files")
        if not isinstance(files, dict) or not files:
            errors.append("artifact.files must be a non-empty path-to-SHA256 object")
        else:
            for relative, file_digest in files.items():
                try:
                    _safe_relative(relative)
                except ComponentArtifactError as error:
                    errors.append(str(error))
                if not isinstance(file_digest, str) or not SHA256_RE.fullmatch(file_digest):
                    errors.append(f"artifact.files[{relative!r}] must be sha256:<64 lowercase hex>")
        uri = artifact.get("uri")
        if not isinstance(uri, str) or not uri.startswith("https://github.com/"):
            errors.append("file artifact uri must be a GitHub HTTPS release asset URL")
        if artifact["kind"] == "native-binary":
            try:
                raw_entrypoint = artifact.get("entrypoint", "")
                if not isinstance(raw_entrypoint, str):
                    raise ComponentArtifactError("unsafe payload path: entrypoint is not a string")
                entrypoint = _safe_relative(raw_entrypoint)
            except ComponentArtifactError as error:
                errors.append("native binary requires a safe entrypoint: " + str(error))
            else:
                if isinstance(files, dict) and entrypoint.as_posix() not in files:
                    errors.append("native binary entrypoint must appear in artifact.files")
                if "executableFiles" in artifact:
                    executable_files = artifact["executableFiles"]
                    if not isinstance(executable_files, list) or not executable_files:
                        errors.append("artifact.executableFiles must be a non-empty array")
                    else:
                        seen_executable_files: set[str] = set()
                        for relative in executable_files:
                            if not isinstance(relative, str):
                                errors.append("artifact.executableFiles entries must be safe relative paths")
                                continue
                            try:
                                _safe_relative(relative)
                            except ComponentArtifactError as error:
                                errors.append(f"artifact.executableFiles contains {error}")
                                continue
                            if relative in seen_executable_files:
                                errors.append("artifact.executableFiles entries must be unique")
                            seen_executable_files.add(relative)
                            if isinstance(files, dict) and relative not in files:
                                errors.append("artifact.executableFiles entries must appear in artifact.files")
                        if entrypoint.as_posix() not in seen_executable_files:
                            errors.append("artifact.executableFiles must include artifact.entrypoint")
    elif artifact["kind"] == "oci-image":
        repository = artifact.get("repository")
        digest = artifact.get("digest")
        platform = artifact.get("platform")
        if not isinstance(repository, str) or not re.fullmatch(r"^[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+$", repository):
            errors.append("OCI artifact.repository must be a canonical image name")
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append("OCI artifact.digest must be sha256:<64 lowercase hex>")
        if not isinstance(platform, dict):
            errors.append("OCI artifact.platform must be an object")
        elif platform.get("os") not in {"linux", "windows"} or platform.get("architecture") not in {
            "amd64",
            "arm64",
        }:
            errors.append("OCI artifact.platform requires supported os and architecture values")

    try:
        _dependency_rows(document.get("dependencies"), "dependencies", strict=schema_version == 2)
    except ComponentArtifactError as error:
        errors.append(str(error))
    if not isinstance(document.get("restart"), dict):
        errors.append("restart must be an object")
    if isinstance(source, dict):
        errors.extend(_validate_provenance(source, document.get("provenance")))
    errors.extend(_validate_compatibility(document.get("compatibility")))
    if schema_version == 2:
        compatibility_value = document.get("compatibility")
        if isinstance(compatibility_value, dict) and not isinstance(compatibility_value.get("groupVersion"), str):
            errors.append("v2 compatibility.groupVersion is required")
        artifact_digest = artifact.get("sha256", artifact.get("digest")) if isinstance(artifact, dict) else None
        if document.get("contentDigest") != artifact_digest:
            errors.append("contentDigest must equal the exact artifact payload digest")
    digest = document.get("manifestDigest")
    if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
        errors.append("manifestDigest must be sha256:<64 lowercase hex>")
    else:
        try:
            if digest != _manifest_digest(document):
                errors.append("manifestDigest does not match the RFC 8785 canonical manifest content")
        except ComponentArtifactError as error:
            errors.append(str(error))
    return list(dict.fromkeys(errors))


def create_manifest(
    descriptor_path: Path,
    output_path: Path,
    artifact_path: Path | None,
    payload_root: Path | None,
) -> dict[str, Any]:
    """Bind a descriptor to exact file bytes and calculate its JCS digest."""
    try:
        descriptor = json.loads(Path(descriptor_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"cannot read descriptor {descriptor_path}: {error}") from error
    if not isinstance(descriptor, dict):
        raise ComponentArtifactError("manifest descriptor must be a JSON object")
    document = dict(descriptor)
    artifact = document.get("artifact")
    if not isinstance(artifact, dict):
        raise ComponentArtifactError("descriptor.artifact must be an object")
    if artifact.get("kind") in ATTACHMENT_KINDS:
        if artifact_path is None or payload_root is None:
            raise ComponentArtifactError("file artifacts require --artifact and --payload-root")
        artifact_file = Path(artifact_path)
        artifact["sha256"] = _sha256_file(artifact_file)
        artifact["sizeBytes"] = artifact_file.stat().st_size
        artifact["files"] = _payload_files(Path(payload_root))
        artifact.setdefault("uri", _release_asset_uri(document, artifact_file.name))
    elif artifact.get("kind") != "oci-image":
        raise ComponentArtifactError("descriptor artifact kind is unsupported")
    if document.get("schemaVersion") == 2:
        document["contentDigest"] = artifact.get("sha256", artifact.get("digest"))
    document["manifestDigest"] = _manifest_digest(document)
    errors = _validate_manifest(document)
    if errors:
        raise ComponentArtifactError("invalid manifest: " + "; ".join(errors))
    output = Path(output_path)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return document


def _read_object(path: Path) -> dict[str, Any]:
    try:
        document = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"cannot read JSON object {path}: {error}") from error
    if not isinstance(document, dict):
        raise ComponentArtifactError(f"expected a JSON object in {path}")
    return document


def _native_manifest_subject_paths(
    manifest_dir: Path,
    *,
    repository: str,
    channel: str,
    source_ref: str,
    source_commit: str,
) -> list[Path]:
    """Validate and return the four required raw native manifest subjects.

    Checksums bind the downloaded manifest bytes themselves, not their JCS
    digest.  This keeps the raw-document attestation distinct from the index.
    """
    root = Path(manifest_dir)
    if root.is_symlink() or not root.is_dir():
        raise ComponentArtifactError(f"native manifest directory is missing or unsafe: {root}")
    source_url = f"https://github.com/{repository}"
    _source_repository_parts(source_url)
    expected_source = {"repository": source_url, "ref": source_ref, "commit": source_commit}
    expected_identities = {
        (component_id, target_version)
        for component_id in RAW_NATIVE_MANIFEST_COMPONENTS
        for target_version in RAW_NATIVE_MANIFEST_TARGETS
    }
    found: dict[tuple[str, str], Path] = {}

    for path in sorted(root.glob("*.manifest.json")):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise ComponentArtifactError(f"native manifest directory contains an unsafe entry: {path.name}")
        document = _read_object(path)
        artifact = document.get("artifact")
        if (
            document.get("componentId") not in RAW_NATIVE_MANIFEST_COMPONENTS
            or not isinstance(artifact, dict)
            or artifact.get("kind") != "native-binary"
        ):
            continue

        errors = _validate_manifest(document)
        if errors:
            raise ComponentArtifactError(f"invalid raw native manifest {path.name}: " + "; ".join(errors))
        if document.get("schemaVersion") != 2:
            raise ComponentArtifactError(f"raw native manifest is not schema v2: {path.name}")
        if (
            document.get("source") != expected_source
            or document.get("channel") != channel
            or document.get("releaseId") != f"{channel}-{source_commit}"
        ):
            raise ComponentArtifactError(f"raw native manifest source identity differs from this release: {path.name}")

        target = document.get("target")
        target_version = target.get("distributionVersion") if isinstance(target, dict) else None
        expected_target = RAW_NATIVE_MANIFEST_TARGETS.get(target_version)
        if expected_target is None or target != expected_target:
            raise ComponentArtifactError(f"unexpected raw native manifest target: {path.name}")
        if path.name != manifest_asset_name(document):
            raise ComponentArtifactError(f"raw native manifest name is not canonical: {path.name}")

        identity = (document["componentId"], target_version)
        if identity not in expected_identities:
            raise ComponentArtifactError(f"unexpected raw native manifest identity: {path.name}")
        if identity in found:
            raise ComponentArtifactError(f"duplicate raw native manifest subject: {identity[0]}/{target_version}")
        found[identity] = path

    missing = sorted(expected_identities - set(found))
    if missing:
        rendered = ", ".join(f"{component_id}/{target_version}" for component_id, target_version in missing)
        raise ComponentArtifactError(f"missing required raw native manifest subjects: {rendered}")
    return sorted(found.values(), key=lambda path: path.name)


def write_native_manifest_subject_checksums(
    manifest_dir: Path,
    output_path: Path,
    *,
    repository: str,
    channel: str,
    source_ref: str,
    source_commit: str,
) -> list[Path]:
    """Write SHA256SUMS-format subjects for the exact raw native manifests.

    The output feeds actions/attest directly.  The ordinary archive and index
    subject files remain separate and keep their existing identities.
    """
    subjects = _native_manifest_subject_paths(
        manifest_dir,
        repository=repository,
        channel=channel,
        source_ref=source_ref,
        source_commit=source_commit,
    )
    rows = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}" for path in subjects]
    destination = Path(output_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text("\n".join(rows) + "\n", encoding="utf-8")
    return subjects


def write_native_manifest_attestation_assets(
    manifest_dir: Path,
    checksum_path: Path,
    bundle_path: Path,
    *,
    repository: str,
    channel: str,
    source_ref: str,
    source_commit: str,
    gh_executable: str = "gh",
    runner: Callable[..., Any] = subprocess.run,
) -> list[Path]:
    """Verify an action bundle against each raw manifest and write release sidecars.

    GitHub CLI verifies the exact source identity from the detached action
    bundle; each release sidecar contains that same single-line bundle.
    """
    subjects = _native_manifest_subject_paths(
        manifest_dir,
        repository=repository,
        channel=channel,
        source_ref=source_ref,
        source_commit=source_commit,
    )
    expected_rows = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}" for path in subjects]
    try:
        actual_checksums = Path(checksum_path).read_text(encoding="utf-8")
    except OSError as error:
        raise ComponentArtifactError(f"cannot read native manifest subject checksums: {error}") from error
    if actual_checksums != "\n".join(expected_rows) + "\n":
        raise ComponentArtifactError("native manifest subject checksum list does not match current raw bytes")

    bundle = Path(bundle_path)
    bundle_info = bundle.lstat()
    if stat.S_ISLNK(bundle_info.st_mode) or not stat.S_ISREG(bundle_info.st_mode):
        raise ComponentArtifactError("detached native manifest bundle is missing or unsafe")
    try:
        bundle_document = json.loads(bundle.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"cannot read detached native manifest bundle: {error}") from error
    if not isinstance(bundle_document, dict) or not isinstance(bundle_document.get("dsseEnvelope"), dict):
        raise ComponentArtifactError("detached native manifest bundle is not a Sigstore DSSE bundle")
    envelope = bundle_document["dsseEnvelope"]
    payload = envelope.get("payload")
    if envelope.get("payloadType") != "application/vnd.in-toto+json" or not isinstance(payload, str):
        raise ComponentArtifactError("detached native manifest bundle has an invalid DSSE payload")
    signatures = envelope.get("signatures")
    if not isinstance(signatures, list) or not signatures:
        raise ComponentArtifactError("detached native manifest bundle has no DSSE signatures")
    try:
        statement = json.loads(base64.b64decode(payload, validate=True))
    except (ValueError, json.JSONDecodeError) as error:
        raise ComponentArtifactError("detached native manifest bundle payload is invalid") from error
    if not isinstance(statement, dict) or statement.get("predicateType") != ATTESTATION_PREDICATE:
        raise ComponentArtifactError("detached native manifest bundle has an unexpected predicate")
    statement_subjects = statement.get("subject")
    if not isinstance(statement_subjects, list):
        raise ComponentArtifactError("detached native manifest bundle has no subject list")
    actual_subjects: dict[str, str] = {}
    for item in statement_subjects:
        if not isinstance(item, dict) or not isinstance(item.get("digest"), dict):
            raise ComponentArtifactError("detached native manifest bundle contains a malformed subject")
        name = item.get("name")
        digest = item["digest"].get("sha256")
        if (
            not isinstance(name, str)
            or not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
            or name in actual_subjects
        ):
            raise ComponentArtifactError("detached native manifest bundle contains an invalid or duplicate subject")
        actual_subjects[name] = digest
    expected_subjects = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in subjects}
    if actual_subjects != expected_subjects:
        raise ComponentArtifactError("detached native manifest bundle subject set differs from raw manifest bytes")

    signer_workflow = f"{repository}/.github/workflows/component-release.yml"
    for subject in subjects:
        command = [
            gh_executable,
            "attestation",
            "verify",
            str(subject),
            "--repo",
            repository,
            "--bundle",
            str(bundle),
            "--signer-workflow",
            signer_workflow,
            "--source-ref",
            source_ref,
            "--source-digest",
            source_commit,
            "--predicate-type",
            ATTESTATION_PREDICATE,
        ]
        try:
            runner(command, check=True, capture_output=True, text=True)
        except subprocess.CalledProcessError as error:
            raise ComponentArtifactError(
                f"GitHub CLI failed to verify detached native manifest bundle for {subject.name}"
            ) from error

    sidecar_bytes = json.dumps(bundle_document, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
    sidecars: list[Path] = []
    for subject in subjects:
        sidecar = subject.with_name(subject.name + ".attestation.jsonl")
        if sidecar.exists() or sidecar.is_symlink():
            raise ComponentArtifactError(f"detached native manifest asset already exists: {sidecar.name}")
        sidecar.write_bytes(sidecar_bytes)
        sidecars.append(sidecar)
    return sidecars


def _verify_asset_uri(document: dict[str, Any]) -> None:
    artifact = document["artifact"]
    if artifact["kind"] == "oci-image":
        return
    expected = _release_asset_uri(document, Path(urlparse(artifact["uri"]).path).name)
    if artifact["uri"] != expected:
        raise ComponentArtifactError("artifact.uri must identify this repository's exact release asset")


def _verify_run(source: dict[str, Any], attestation: dict[str, Any]) -> None:
    """Verify workflow run identity over HTTPS against the GitHub Actions API."""
    run = attestation["run"]
    owner, repository = _source_repository_parts(source["repository"])
    expected_url = f"https://github.com/{owner}/{repository}/actions/runs/{run['id']}/attempts/{run['attempt']}"
    if run["url"] != expected_url:
        raise ComponentArtifactError("provenance.run.url does not match source repository and run identity")
    api_url = f"https://api.github.com/repos/{owner}/{repository}/actions/runs/{run['id']}/attempts/{run['attempt']}"
    run_document = _github_json(api_url)

    run_repository = run_document.get("repository", {}).get("full_name")
    workflow_path = attestation["workflow"].split("/", 2)[-1]
    actual_workflow = str(run_document.get("path", "")).split("@", 1)[0]
    source_ref = source["ref"]
    actual_ref = run_document.get("head_branch")
    expected_ref = source_ref.removeprefix("refs/heads/").removeprefix("refs/tags/")
    checks = {
        "repository": run_repository == f"{owner}/{repository}",
        "commit": run_document.get("head_sha") == source["commit"],
        "attempt": run_document.get("run_attempt") == run["attempt"],
        "ref": actual_ref == expected_ref,
        "workflow": actual_workflow == workflow_path,
        "completed": run_document.get("status") == "completed",
        "successful": run_document.get("conclusion") == "success",
    }
    failed = sorted(name for name, passed in checks.items() if not passed)
    if failed:
        raise ComponentArtifactError("GitHub Actions run did not match source pins: " + ", ".join(failed))


def _github_json(url: str) -> dict[str, Any]:
    """Fetch one fixed GitHub API object over HTTPS using optional GH token."""
    headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            document = json.loads(response.read().decode("utf-8"))
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
        raise ComponentArtifactError(f"cannot verify GitHub API identity over HTTPS: {error}") from error
    if not isinstance(document, dict):
        raise ComponentArtifactError("GitHub API returned an unexpected JSON document")
    return document


def _publisher_for(catalog: dict[str, Any], repository: str, workflow: str) -> dict[str, Any]:
    publishers = catalog.get("publishers")
    if not isinstance(publishers, list):
        raise ComponentArtifactError("trusted catalog has no publishers list")
    for publisher in publishers:
        if isinstance(publisher, dict) and publisher.get("repository") == repository:
            if publisher.get("workflow") != workflow:
                raise ComponentArtifactError("producer workflow does not match trusted catalog pin")
            return publisher
    raise ComponentArtifactError("producer repository is not trusted by the component catalog")


def _verify_channel_pin(catalog: dict[str, Any], channel: str, source_ref: str) -> None:
    channels = catalog.get("channels")
    policy = channels.get(channel) if isinstance(channels, dict) else None
    refs = policy.get("sourceRefs") if isinstance(policy, dict) else None
    if not isinstance(refs, list) or source_ref not in refs:
        raise ComponentArtifactError("source.ref is not allowed for this channel by the trusted catalog")


def _declared_targets_for_json(
    catalog: dict[str, Any],
    component: dict[str, Any],
    target: dict[str, Any],
    artifact_kind: str | None = None,
) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    declarations = component.get("targets")
    global_targets = catalog.get("targets")
    if not isinstance(declarations, list) or not isinstance(global_targets, list):
        return []
    matches: list[tuple[dict[str, Any], dict[str, Any]]] = []
    for declaration in declarations:
        if not isinstance(declaration, dict) or (
            artifact_kind is not None and declaration.get("artifactKind") != artifact_kind
        ):
            continue
        for global_target in global_targets:
            if (
                isinstance(global_target, dict)
                and global_target.get("id") == declaration.get("targetId")
                and global_target.get("target") == target
            ):
                matches.append((declaration, global_target))
    return matches


def _component_target(
    catalog: dict[str, Any], component_id: str, artifact: dict[str, Any], target: dict[str, Any]
) -> dict[str, Any]:
    components = catalog.get("components")
    component = next(
        (row for row in components or [] if isinstance(row, dict) and row.get("componentId") == component_id),
        None,
    )
    if not isinstance(component, dict):
        raise ComponentArtifactError(f"component is not declared by trusted catalog: {component_id}")
    artifact_kind = artifact.get("kind")
    if component.get("artifactKinds", [component.get("kind")]).count(artifact_kind) != 1:
        raise ComponentArtifactError("artifact kind is not allowed for this component by the catalog")
    global_targets = catalog.get("targets")
    if not isinstance(global_targets, list) or not any(
        isinstance(row, dict) and row.get("target") == target for row in global_targets
    ):
        raise ComponentArtifactError("manifest target is not defined by trusted catalog")
    build_dependency = component.get("role") == "build-dependency"
    publication_target = artifact_kind == "oci-image"
    permitted_support = {"supported", "contract-only"} if build_dependency or publication_target else {"supported"}
    matches = _declared_targets_for_json(catalog, component, target, artifact_kind)
    if len(matches) != 1 or matches[0][0].get("support") not in permitted_support:
        raise ComponentArtifactError("target is not authorized for this component by the catalog")
    host_target = matches[0][1]
    if not isinstance(host_target, dict) or host_target.get("hostSupport") not in permitted_support:
        raise ComponentArtifactError("host target is not authorized by the catalog")
    if artifact_kind == "oci-image" and component.get("ociImageRepository") != artifact.get("repository"):
        raise ComponentArtifactError("OCI repository does not match the component catalog pin")
    return component


def _component_has_supported_target(catalog: dict[str, Any], component_id: str, target: dict[str, Any]) -> bool:
    components = catalog.get("components")
    component = next(
        (row for row in components or [] if isinstance(row, dict) and row.get("componentId") == component_id),
        None,
    )
    if not isinstance(component, dict):
        return False
    build_dependency = component.get("role") == "build-dependency"
    matches = _declared_targets_for_json(catalog, component, target)
    if not matches or len({row.get("targetId") for row, _ in matches}) != 1:
        return False
    for row, global_target in matches:
        permitted_support = (
            {"supported", "contract-only"}
            if build_dependency or row.get("artifactKind") == "oci-image"
            else {"supported"}
        )
        if row.get("support") in permitted_support and global_target.get("hostSupport") in permitted_support:
            return True
    return False


def _verify_contract_lock(compatibility: dict[str, Any]) -> None:
    lock = compatibility["contractLock"]
    owner, repository = lock["repository"].split("/", 1)
    url = f"https://raw.githubusercontent.com/{owner}/{repository}/{lock['commit']}/{quote(lock['path'], safe='/')}"
    request = urllib.request.Request(url, headers={"Accept": "application/vnd.github.raw+json"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            raw = response.read()
    except (OSError, urllib.error.URLError) as error:
        raise ComponentArtifactError(f"cannot fetch pinned compatibility lock over HTTPS: {error}") from error
    actual = "sha256:" + hashlib.sha256(raw).hexdigest()
    if actual != lock["sha256"]:
        raise ComponentArtifactError("tracked compatibility lock bytes do not match contractLock.sha256")


def trusted_catalog_compatibility(
    catalog_path: Path,
    catalog: dict[str, Any],
    component: dict[str, Any],
) -> dict[str, Any] | None:
    """Build compatibility metadata only from the pinned catalog and lock commit.

    The source repository's Product bundle lock is deliberately not consulted
    here: it governs data-bundle inputs, while this pin governs shared wire/API
    compatibility for independently released Platform components.
    """
    group_id = component.get("compatibilityGroup")
    if group_id is None:
        return None
    groups = catalog.get("compatibilityGroups")
    if not isinstance(groups, list):
        raise ComponentArtifactError("trusted catalog has no compatibilityGroups array")
    group = next(
        (row for row in groups if isinstance(row, dict) and row.get("groupId") == group_id),
        None,
    )
    if not isinstance(group, dict):
        raise ComponentArtifactError(
            f"component references an unknown compatibility group: {component.get('componentId')}"
        )
    lock = group.get("contractLock")
    if not isinstance(lock, dict):
        raise ComponentArtifactError("trusted compatibility group has no immutable contractLock pin")
    if group_id == PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID:
        if any(
            group.get(key) != PACKAGE_RUNTIME_COMPATIBILITY[key]
            for key in ("groupId", "groupVersion", "contractApiVersion", "wireApiVersion", "contractLock")
        ):
            raise ComponentArtifactError(
                "trusted Package Runtime compatibility does not match its frozen protocol lock"
            )
    elif group_id != "workspace-product-v2":
        raise ComponentArtifactError(f"unsupported trusted compatibility group: {group_id}")
    if lock.get("repository") != "DoHorizon-AI/Cyrene-Workspace":
        raise ComponentArtifactError("trusted protocol lock must be pinned to Cyrene-Workspace")
    commit = lock.get("commit")
    path = lock.get("path")
    digest = lock.get("sha256")
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ComponentArtifactError("trusted contractLock commit must be a full lower-case Git SHA")
    expected_path = (
        PACKAGE_RUNTIME_COMPATIBILITY["contractLock"]["path"]
        if group_id == PACKAGE_RUNTIME_COMPATIBILITY_GROUP_ID
        else "governance/workspace-connection-protocols-v2.lock.json"
    )
    if not isinstance(path, str) or path != expected_path:
        raise ComponentArtifactError("trusted contractLock path is not the frozen protocol lock for its group")
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ComponentArtifactError("trusted contractLock sha256 must be sha256:<64 lowercase hex>")

    # Signed catalog downloads need no checkout; lock bytes remain pinned.
    # 独立下载的目录仍按固定来源与摘要校验协议锁。
    _verify_contract_lock({"contractLock": lock})

    for key in ("groupId", "groupVersion", "contractApiVersion", "wireApiVersion"):
        if not isinstance(group.get(key), str) or not group[key]:
            raise ComponentArtifactError(f"trusted compatibility group {key} must be a non-empty string")
    return {
        "groupId": group["groupId"],
        "groupVersion": group["groupVersion"],
        "contractApiVersion": group["contractApiVersion"],
        "wireApiVersion": group["wireApiVersion"],
        "contractLock": dict(lock),
    }


def _verify_manifest_catalog_metadata(
    document: dict[str, Any], catalog: dict[str, Any]
) -> tuple[str, str, dict[str, Any]]:
    """Verify manifest declarations against the trusted catalog without release API calls.

    This check is safe before a draft is published and includes exact component,
    target, dependency, compatibility lock, source channel, and publisher pins.
    在发布前只依赖可信目录验证组件元数据，不要求release已存在。
    """
    source = document["source"]
    attestation = document["provenance"]["attestation"]
    owner, repository_name = _source_repository_parts(source["repository"])
    repository = f"{owner}/{repository_name}"
    publisher = _publisher_for(catalog, repository, attestation["workflow"])
    _verify_channel_pin(catalog, document["channel"], source["ref"])
    component = _component_target(catalog, document["componentId"], document["artifact"], document["target"])
    if component.get("publisher") != repository:
        raise ComponentArtifactError("component publisher does not match manifest source.repository")
    strict_dependencies = document.get("schemaVersion") == 2
    actual_dependencies = _dependency_rows(
        document.get("dependencies"), "manifest dependencies", strict=strict_dependencies
    )
    expected_dependencies = _dependency_rows(
        component.get("dependencies"), "catalog dependencies", strict=strict_dependencies
    )
    if actual_dependencies != expected_dependencies:
        raise ComponentArtifactError("manifest dependencies do not match the trusted component catalog")
    compatibility_groups = catalog.get("compatibilityGroups", [])
    expected_groups = [
        group
        for group in compatibility_groups
        if isinstance(group, dict)
        and any(
            isinstance(member, dict) and member.get("componentId") == document["componentId"]
            for member in group.get("members", [])
        )
    ]
    if len(expected_groups) > 1:
        raise ComponentArtifactError("component belongs to multiple trusted compatibility groups")
    group_definition = expected_groups[0] if expected_groups else None
    expected_group = group_definition.get("groupId") if isinstance(group_definition, dict) else None
    compatibility = document.get("compatibility")
    if expected_group is not None:
        if not isinstance(compatibility, dict) or compatibility.get("groupId") != expected_group:
            raise ComponentArtifactError("manifest omits its trusted compatibility group pin")
        if not isinstance(group_definition, dict):
            raise ComponentArtifactError("trusted compatibility group is malformed")
        if compatibility.get("contractApiVersion") != group_definition.get("contractApiVersion") or compatibility.get(
            "wireApiVersion"
        ) != group_definition.get("wireApiVersion"):
            raise ComponentArtifactError("manifest compatibility API versions do not match the catalog")
        if compatibility.get("groupVersion") != group_definition.get("groupVersion"):
            raise ComponentArtifactError("manifest compatibility groupVersion does not match the catalog")
        _verify_contract_lock(compatibility)
    elif compatibility is not None:
        raise ComponentArtifactError("manifest declares an unknown compatibility group")
    if document.get("schemaVersion") == 2:
        expected_protocol = component.get("protocolVersion")
        if expected_protocol is None and expected_group is not None:
            expected_member = next(
                (
                    member
                    for member in group_definition.get("members", [])
                    if isinstance(member, dict) and member.get("componentId") == document["componentId"]
                ),
                None,
            )
            expected_protocol = expected_member.get("protocolVersion") if isinstance(expected_member, dict) else None
        if document.get("protocolVersion") != expected_protocol:
            raise ComponentArtifactError("manifest protocolVersion does not match the trusted component or group pin")
    expected_ref_tag = f"{document['channel']}-{source['commit']}"
    if document["releaseId"] != expected_ref_tag:
        raise ComponentArtifactError("releaseId must be the immutable channel plus full source commit")
    return owner, repository_name, publisher


def _verify_release_metadata(document: dict[str, Any], catalog: dict[str, Any], manifest_path: Path) -> None:
    owner, repository_name, publisher = _verify_manifest_catalog_metadata(document, catalog)
    api_url = (
        f"https://api.github.com/repos/{owner}/{repository_name}/releases/tags/{quote(document['releaseId'], safe='-')}"
    )
    release = _github_json(api_url)
    expected_prerelease = document["channel"] == "preview"
    if release.get("tag_name") != document["releaseId"] or release.get("prerelease") is not expected_prerelease:
        raise ComponentArtifactError("GitHub Release tag/prerelease state does not match trusted channel")
    if release.get("draft") is not False or release.get("immutable") is not True:
        raise ComponentArtifactError("release must be published and immutable")
    discovery = publisher.get("releaseDiscovery", {})
    index_asset_name = discovery.get("indexAssetName") if isinstance(discovery, dict) else None
    if index_asset_name != "component-release-index-v1.json":
        raise ComponentArtifactError("trusted publisher has an unexpected release index asset name")
    names = {asset.get("name") for asset in release.get("assets", []) if isinstance(asset, dict)}
    if manifest_path.name not in names:
        raise ComponentArtifactError("immutable GitHub Release is missing the verified manifest asset")
    if document["artifact"]["kind"] in ATTACHMENT_KINDS:
        artifact_name = Path(urlparse(document["artifact"]["uri"]).path).name
        if artifact_name not in names:
            raise ComponentArtifactError("immutable GitHub Release is missing the component payload asset")


def _verify_index_catalog_metadata(
    document: dict[str, Any], catalog: dict[str, Any]
) -> tuple[str, str, dict[str, Any]]:
    """Verify publisher-local index entries against trusted catalog pins.

    A publisher index may contain only releases that publisher produced. Global
    adoption completeness is enforced by the update/apply layer across publishers.
    校验当前publisher实际发布的行；跨publisher整组采纳由更新层统一门禁。
    """
    source = document["source"]
    attestation = document["provenance"]["attestation"]
    owner, repository_name = _source_repository_parts(source["repository"])
    repository = f"{owner}/{repository_name}"
    if document.get("repository") != repository:
        raise ComponentArtifactError("index repository does not match the trusted source repository")
    publisher = _publisher_for(catalog, repository, attestation["workflow"])
    _verify_channel_pin(catalog, document["channel"], source["ref"])
    expected_release_id = f"{document['channel']}-{source['commit']}"
    for row in document["releases"]:
        component_rows = [
            component
            for component in catalog.get("components", [])
            if isinstance(component, dict) and component.get("componentId") == row["componentId"]
        ]
        if len(component_rows) != 1 or component_rows[0].get("publisher") != repository:
            raise ComponentArtifactError(
                f"index component publisher does not match the index repository: {row['componentId']}"
            )
        if not _component_has_supported_target(catalog, row["componentId"], row["target"]):
            raise ComponentArtifactError(f"index contains unsupported component target: {row['componentId']}")
        expected_uri = _release_asset_uri(
            {
                "source": {"repository": source["repository"]},
                "releaseId": expected_release_id,
            },
            row["manifestUri"].rsplit("/", 1)[-1],
        )
        if row["manifestUri"] != expected_uri:
            raise ComponentArtifactError(
                "index manifestUri does not identify this publisher's exact immutable release asset"
            )
    catalog_groups = {
        group.get("groupId"): group for group in catalog.get("compatibilityGroups", []) if isinstance(group, dict)
    }
    for group in document["compatibilityGroups"]:
        definition = catalog_groups.get(group["groupId"])
        if not isinstance(definition, dict):
            raise ComponentArtifactError("index declares a compatibility group absent from the catalog")
        if group["contractApiVersion"] != definition.get("contractApiVersion") or group[
            "wireApiVersion"
        ] != definition.get("wireApiVersion"):
            raise ComponentArtifactError("index compatibility versions do not match the catalog")
        if group.get("groupVersion") != definition.get("groupVersion"):
            raise ComponentArtifactError("index compatibility groupVersion does not match the catalog")
        if group.get("contractLock") != definition.get("contractLock"):
            raise ComponentArtifactError("index compatibility lock does not match the trusted catalog")
        _verify_contract_lock({"contractLock": definition.get("contractLock")})
        declared_members = {
            member.get("componentId") for member in definition.get("members", []) if isinstance(member, dict)
        }
        if any(member.get("componentId") not in declared_members for member in group["members"]):
            raise ComponentArtifactError("index compatibility group contains a component absent from the catalog")
    return owner, repository_name, publisher


def _verify_index_metadata(document: dict[str, Any], catalog: dict[str, Any]) -> None:
    owner, repository_name, publisher = _verify_index_catalog_metadata(document, catalog)
    release_id = f"{document['channel']}-{document['source']['commit']}"
    release = _github_json(
        f"https://api.github.com/repos/{owner}/{repository_name}/releases/tags/{quote(release_id, safe='-')}"
    )
    expected_prerelease = document["channel"] == "preview"
    if release.get("tag_name") != release_id or release.get("prerelease") is not expected_prerelease:
        raise ComponentArtifactError("GitHub Release tag/prerelease state does not match trusted channel")
    if release.get("draft") is not False or release.get("immutable") is not True:
        raise ComponentArtifactError("release must be published and immutable")
    discovery = publisher.get("releaseDiscovery", {})
    index_asset_name = discovery.get("indexAssetName") if isinstance(discovery, dict) else None
    asset_names = {row.get("name") for row in release.get("assets", []) if isinstance(row, dict)}
    if index_asset_name != "component-release-index-v1.json" or index_asset_name not in asset_names:
        raise ComponentArtifactError("immutable GitHub Release lacks the trusted index asset")
    manifest_asset_names = {Path(urlparse(row["manifestUri"]).path).name for row in document["releases"]}
    if not manifest_asset_names.issubset(asset_names):
        raise ComponentArtifactError("immutable GitHub Release is missing an indexed component manifest asset")


def _verify_attestation(
    subject: str,
    source: dict[str, Any],
    attestation: dict[str, Any],
    *,
    subject_name: str | None,
) -> None:
    """Verify an artifact or OCI digest using the GitHub CLI trust policy."""
    owner, repository = _source_repository_parts(source["repository"])
    command = [
        "gh",
        "attestation",
        "verify",
        subject,
        "--repo",
        f"{owner}/{repository}",
        "--signer-workflow",
        attestation["workflow"],
        "--source-ref",
        source["ref"],
        "--source-digest",
        source["commit"],
        "--predicate-type",
        ATTESTATION_PREDICATE,
        "--format",
        "json",
    ]
    try:
        result = subprocess.run(command, check=True, capture_output=True, text=True)
    except FileNotFoundError as error:
        raise ComponentArtifactError("GitHub CLI is required for attestation verification") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip()
        raise ComponentArtifactError(f"GitHub attestation verification failed: {detail}") from error
    if subject_name:
        try:
            verified = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise ComponentArtifactError("GitHub CLI returned invalid JSON verification output") from error
        statements = (
            [item.get("verificationResult", {}).get("statement", {}) for item in verified if isinstance(item, dict)]
            if isinstance(verified, list)
            else []
        )
        if not any(
            subject.get("name") == subject_name
            for statement in statements
            for subject in statement.get("subject", [])
            if isinstance(subject, dict)
        ):
            raise ComponentArtifactError("verified attestation subjectName does not match the manifest")


def verify_manifest(
    manifest_path: Path,
    artifact_path: Path | None,
    *,
    verify_run: bool,
    verify_attestation: bool,
    verify_release: bool = False,
    verify_attestation_only: bool = False,
    catalog_path: Path | None = None,
    manifest_uri: str | None = None,
) -> dict[str, Any]:
    """Validate manifest structure, payload bytes, and optional provenance."""
    document = _read_object(manifest_path)
    errors = _validate_manifest(document)
    if errors:
        raise ComponentArtifactError("invalid manifest: " + "; ".join(errors))
    _verify_asset_uri(document)
    if manifest_uri is not None:
        expected_manifest_uri = _release_asset_uri(document, Path(manifest_path).name)
        if manifest_uri != expected_manifest_uri:
            raise ComponentArtifactError("index manifestUri does not match the downloaded manifest asset")
    artifact = document["artifact"]
    if artifact["kind"] in ATTACHMENT_KINDS:
        if artifact_path is None:
            raise ComponentArtifactError("file artifact verification requires --artifact")
        path = Path(artifact_path)
        if path.stat().st_size != artifact["sizeBytes"]:
            raise ComponentArtifactError("artifact size does not match manifest")
        if _sha256_file(path) != artifact["sha256"]:
            raise ComponentArtifactError("artifact digest does not match manifest")
        if _archive_files(path) != artifact["files"]:
            raise ComponentArtifactError("artifact file map does not match manifest")
    if verify_attestation and verify_attestation_only:
        raise ComponentArtifactError("full and prepublication attestation verification are mutually exclusive")
    catalog: dict[str, Any] | None = None
    if catalog_path is not None or verify_release or verify_attestation_only or verify_run or verify_attestation:
        if catalog_path is None:
            raise ComponentArtifactError("catalog or provenance verification requires --catalog")
        catalog = _read_object(catalog_path)
    if catalog is not None:
        _verify_manifest_catalog_metadata(document, catalog)
    if verify_release or verify_run or verify_attestation:
        if catalog is None:
            raise ComponentArtifactError("release verification requires --catalog")
        _verify_release_metadata(document, catalog, Path(manifest_path))
    if verify_run or verify_attestation:
        _verify_run(document["source"], document["provenance"]["attestation"])
    if verify_attestation or verify_attestation_only:
        if artifact["kind"] == "oci-image":
            subject = f"oci://{artifact['repository']}@{artifact['digest']}"
            subject_name = artifact["repository"]
        else:
            if artifact_path is None:
                raise ComponentArtifactError("file artifact attestation verification requires --artifact")
            subject = str(artifact_path)
            subject_name = document["provenance"]["attestation"]["subjectName"]
        _verify_attestation(
            subject,
            document["source"],
            document["provenance"]["attestation"],
            subject_name=subject_name,
        )
    return document


def _validate_index(document: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    required = {
        "schemaVersion",
        "repository",
        "channel",
        "generatedAt",
        "source",
        "provenance",
        "releases",
        "indexDigest",
        "compatibilityGroups",
    }
    missing = sorted(required - set(document))
    if missing:
        errors.append("missing index keys: " + ", ".join(missing))
    if document.get("schemaVersion") != 1:
        errors.append("schemaVersion must be 1")
    if document.get("channel") not in {"stable", "preview"}:
        errors.append("channel must be stable or preview")
    repository = document.get("repository")
    source = document.get("source")
    if not isinstance(source, dict):
        errors.append("source must be an object")
    else:
        try:
            owner, repo = _source_repository_parts(source.get("repository", ""))
            if repository != f"{owner}/{repo}":
                errors.append("index.repository must match source.repository")
        except ComponentArtifactError as error:
            errors.append(str(error))
        if not isinstance(source.get("commit"), str) or not SOURCE_COMMIT_RE.fullmatch(source["commit"]):
            errors.append("index source.commit must be a full lower-case Git SHA")
        if not isinstance(source.get("ref"), str) or not re.fullmatch(
            r"refs/(heads|tags)/[A-Za-z0-9._/-]+", source["ref"]
        ):
            errors.append("index source.ref must identify a Git branch or tag")
        errors.extend(_validate_provenance(source, document.get("provenance")))
    if not isinstance(document.get("generatedAt"), str):
        errors.append("generatedAt must be an RFC 3339 timestamp")
    else:
        try:
            dt.datetime.fromisoformat(document["generatedAt"].replace("Z", "+00:00"))
        except ValueError:
            errors.append("generatedAt must be an RFC 3339 timestamp")
    releases = document.get("releases")
    if not isinstance(releases, list) or not releases:
        errors.append("releases must be a non-empty array")
    else:
        seen: set[tuple[str, bytes]] = set()
        for release in releases:
            if not isinstance(release, dict):
                errors.append("each release row must be an object")
                continue
            if not COMPONENT_ID_RE.fullmatch(str(release.get("componentId", ""))):
                errors.append("release.componentId is invalid")
            if not isinstance(release.get("version"), str) or not release["version"]:
                errors.append("release.version must be a non-empty string")
            if not isinstance(release.get("manifestUri"), str) or not release["manifestUri"].startswith(
                "https://github.com/"
            ):
                errors.append("release.manifestUri must be an immutable GitHub release asset URL")
            if not isinstance(release.get("manifestDigest"), str) or not SHA256_RE.fullmatch(release["manifestDigest"]):
                errors.append("release.manifestDigest must be sha256:<64 lowercase hex>")
            errors.extend(_validate_target(release.get("target")))
            try:
                identity = (str(release.get("componentId")), _canonical_json(release.get("target")))
                if identity in seen:
                    errors.append("index contains duplicate component and target rows")
                seen.add(identity)
            except ComponentArtifactError as error:
                errors.append(str(error))
    groups = document.get("compatibilityGroups")
    if not isinstance(groups, list):
        errors.append("compatibilityGroups must be an array")
    else:
        release_rows: set[tuple[Any, bytes, Any, Any]] = set()
        if isinstance(releases, list):
            for release in releases:
                if not isinstance(release, dict):
                    continue
                try:
                    release_rows.add(
                        (
                            release.get("componentId"),
                            _canonical_json(release.get("target")),
                            release.get("version"),
                            release.get("manifestDigest"),
                        )
                    )
                except ComponentArtifactError as error:
                    errors.append(str(error))
        group_ids: set[str] = set()
        for group in groups:
            if not isinstance(group, dict):
                errors.append("each compatibility group must be an object")
                continue
            group_id = group.get("groupId")
            if not isinstance(group_id, str) or not group_id or group_id in group_ids:
                errors.append("compatibility group IDs must be non-empty and unique")
            else:
                group_ids.add(group_id)
            errors.extend(
                _validate_compatibility(
                    {
                        "groupId": group.get("groupId"),
                        "groupVersion": group.get("groupVersion"),
                        "contractApiVersion": group.get("contractApiVersion"),
                        "wireApiVersion": group.get("wireApiVersion"),
                        "contractLock": group.get("contractLock"),
                    }
                )
            )
            members = group.get("members")
            if not isinstance(members, list) or not members:
                errors.append("compatibility group members must be a non-empty array")
                continue
            member_ids: set[tuple[str, bytes]] = set()
            for member in members:
                if not isinstance(member, dict):
                    errors.append("compatibility group members must be objects")
                    continue
                component_id = member.get("componentId")
                target = member.get("target")
                version = member.get("version")
                manifest_digest = member.get("manifestDigest")
                try:
                    target_bytes = _canonical_json(target)
                    if not isinstance(component_id, str) or not COMPONENT_ID_RE.fullmatch(component_id):
                        raise ComponentArtifactError("member componentId is invalid")
                    if not isinstance(manifest_digest, str) or not SHA256_RE.fullmatch(manifest_digest):
                        raise ComponentArtifactError("member manifestDigest is invalid")
                    identity = (component_id, target_bytes)
                    if identity in member_ids:
                        errors.append("compatibility group repeats a component target")
                    member_ids.add(identity)
                    if (component_id, target_bytes, version, manifest_digest) not in release_rows:
                        errors.append("compatibility group member does not match an indexed release")
                except ComponentArtifactError as error:
                    errors.append(str(error))
    digest = document.get("indexDigest")
    if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
        errors.append("indexDigest must be sha256:<64 lowercase hex>")
    else:
        try:
            if digest != _index_digest(document):
                errors.append("indexDigest does not match the RFC 8785 canonical index content")
        except ComponentArtifactError as error:
            errors.append(str(error))
    return list(dict.fromkeys(errors))


def create_index(
    manifest_paths: list[Path],
    output_path: Path,
    *,
    channel: str,
    source_ref: str,
    source_commit: str,
    run_id: str,
    run_attempt: int,
    workflow: str,
    attestation_uri: str | None,
    generated_at: str | None = None,
) -> dict[str, Any]:
    """Create one immutable repository/channel index over validated manifests."""
    if not manifest_paths:
        raise ComponentArtifactError("release index requires at least one component manifest")
    source_repositories: set[str] = set()
    release_ids: set[str] = set()
    releases: list[dict[str, Any]] = []
    seen: set[tuple[str, bytes]] = set()
    group_metadata: dict[str, dict[str, Any]] = {}
    for path in sorted(manifest_paths):
        manifest = _read_object(path)
        errors = _validate_manifest(manifest)
        if errors:
            raise ComponentArtifactError(f"invalid manifest {path}: " + "; ".join(errors))
        if manifest["channel"] != channel:
            raise ComponentArtifactError(f"manifest {path} does not match index channel")
        if manifest["source"]["ref"] != source_ref or manifest["source"]["commit"] != source_commit:
            raise ComponentArtifactError(f"manifest {path} does not match index source pins")
        source_repositories.add(manifest["source"]["repository"])
        release_ids.add(manifest["releaseId"])
        identity = (manifest["componentId"], _canonical_json(manifest["target"]))
        if identity in seen:
            raise ComponentArtifactError(f"duplicate component target in index: {identity[0]}")
        seen.add(identity)
        releases.append(
            {
                "componentId": manifest["componentId"],
                "version": manifest["version"],
                "target": manifest["target"],
                "manifestUri": _release_asset_uri(manifest, manifest_asset_name(manifest)),
                "manifestDigest": manifest["manifestDigest"],
            }
        )
        compatibility = manifest.get("compatibility")
        if compatibility is not None:
            group_id = compatibility["groupId"]
            metadata = {
                "groupId": group_id,
                "groupVersion": compatibility["groupVersion"],
                "contractApiVersion": compatibility["contractApiVersion"],
                "wireApiVersion": compatibility["wireApiVersion"],
                "contractLock": compatibility["contractLock"],
                "members": [],
            }
            previous = group_metadata.get(group_id)
            if previous is not None and _canonical_json(
                {key: value for key, value in previous.items() if key != "members"}
            ) != _canonical_json({key: value for key, value in metadata.items() if key != "members"}):
                raise ComponentArtifactError(f"compatibility pins differ within group {group_id}")
            group_metadata[group_id] = previous or metadata
            group_metadata[group_id]["members"].append(
                {
                    "componentId": manifest["componentId"],
                    "target": manifest["target"],
                    "version": manifest["version"],
                    "manifestDigest": manifest["manifestDigest"],
                }
            )
    if len(source_repositories) != 1 or len(release_ids) != 1:
        raise ComponentArtifactError("all indexed components must share one repository and immutable releaseId")
    owner, repository_name = _source_repository_parts(next(iter(source_repositories)))
    if not run_id.isdigit() or run_attempt < 1:
        raise ComponentArtifactError("GitHub run identity is invalid")
    run = {
        "id": run_id,
        "attempt": run_attempt,
        "url": f"https://github.com/{owner}/{repository_name}/actions/runs/{run_id}/attempts/{run_attempt}",
    }
    attestation: dict[str, Any] = {
        "kind": "github-artifact-attestation",
        "subjectName": "component-release-index-v1.json",
        "repository": f"{owner}/{repository_name}",
        "workflow": workflow,
        "predicateType": ATTESTATION_PREDICATE,
        "run": run,
    }
    if attestation_uri:
        attestation["uri"] = attestation_uri
    index = {
        "schemaVersion": 1,
        "repository": f"{owner}/{repository_name}",
        "channel": channel,
        "generatedAt": generated_at
        or dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "source": {
            "repository": next(iter(source_repositories)),
            "ref": source_ref,
            "commit": source_commit,
        },
        "provenance": {"attestation": attestation},
        "releases": releases,
        "compatibilityGroups": [group_metadata[key] for key in sorted(group_metadata)],
    }
    index["indexDigest"] = _index_digest(index)
    errors = _validate_index(index)
    if errors:
        raise ComponentArtifactError("invalid index: " + "; ".join(errors))
    output = Path(output_path)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(index, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return index


def verify_index(
    index_path: Path,
    *,
    verify_run: bool,
    verify_attestation: bool,
    verify_release: bool = False,
    verify_attestation_only: bool = False,
    catalog_path: Path | None = None,
) -> dict[str, Any]:
    """Verify index digest and optional GHA run/attestation against exact bytes."""
    document = _read_object(index_path)
    errors = _validate_index(document)
    if errors:
        raise ComponentArtifactError("invalid index: " + "; ".join(errors))
    if verify_attestation and verify_attestation_only:
        raise ComponentArtifactError("full and prepublication attestation verification are mutually exclusive")
    catalog: dict[str, Any] | None = None
    if catalog_path is not None or verify_release or verify_attestation_only or verify_run or verify_attestation:
        if catalog_path is None:
            raise ComponentArtifactError("catalog or provenance verification requires --catalog")
        catalog = _read_object(catalog_path)
    if catalog is not None:
        _verify_index_catalog_metadata(document, catalog)
    if verify_release or verify_run or verify_attestation:
        if catalog is None:
            raise ComponentArtifactError("release verification requires --catalog")
        _verify_index_metadata(document, catalog)
    if verify_run or verify_attestation:
        _verify_run(document["source"], document["provenance"]["attestation"])
    if verify_attestation or verify_attestation_only:
        attestation = document["provenance"]["attestation"]
        _verify_attestation(
            str(index_path),
            document["source"],
            attestation,
            subject_name=attestation["subjectName"],
        )
    return document


def _pack_command(arguments: argparse.Namespace) -> int:
    _write_deterministic_tar_gz(arguments.payload_root, arguments.output)
    print(f"packed {arguments.payload_root} -> {arguments.output} ({_sha256_file(arguments.output)})")
    return 0


def _create_command(arguments: argparse.Namespace) -> int:
    document = create_manifest(arguments.descriptor, arguments.output, arguments.artifact, arguments.payload_root)
    print(f"created {arguments.output} ({document['manifestDigest']})")
    return 0


def _verify_command(arguments: argparse.Namespace) -> int:
    document = verify_manifest(
        arguments.manifest,
        arguments.artifact,
        verify_run=arguments.verify_run,
        verify_attestation=arguments.verify_attestation,
        verify_release=arguments.verify_release,
        verify_attestation_only=arguments.verify_attestation_only,
        catalog_path=arguments.catalog,
        manifest_uri=arguments.manifest_uri,
    )
    print(f"verified {document['componentId']} {document['version']} {document['manifestDigest']}")
    return 0


def _index_command(arguments: argparse.Namespace) -> int:
    manifests = list(arguments.manifest)
    if arguments.manifest_dir:
        manifests.extend(sorted(Path(arguments.manifest_dir).glob("*.manifest.json")))
    index = create_index(
        manifests,
        arguments.output,
        channel=arguments.channel,
        source_ref=arguments.source_ref,
        source_commit=arguments.source_commit,
        run_id=arguments.run_id,
        run_attempt=arguments.run_attempt,
        workflow=arguments.workflow,
        attestation_uri=arguments.attestation_uri,
    )
    print(f"created {arguments.output} ({len(index['releases'])} releases; {index['indexDigest']})")
    return 0


def _native_manifest_subjects_command(arguments: argparse.Namespace) -> int:
    """Write the checksums for the exact four supported raw native manifests."""
    subjects = write_native_manifest_subject_checksums(
        arguments.manifest_dir,
        arguments.output,
        repository=arguments.repository,
        channel=arguments.channel,
        source_ref=arguments.source_ref,
        source_commit=arguments.source_commit,
    )
    print(f"created {arguments.output} ({len(subjects)} raw native manifest subjects)")
    return 0


def _native_manifest_attestation_assets_command(arguments: argparse.Namespace) -> int:
    """Verify a detached multi-subject bundle and materialize named sidecars."""
    sidecars = write_native_manifest_attestation_assets(
        arguments.manifest_dir,
        arguments.checksum_file,
        arguments.bundle,
        repository=arguments.repository,
        channel=arguments.channel,
        source_ref=arguments.source_ref,
        source_commit=arguments.source_commit,
        gh_executable=arguments.gh_executable,
    )
    print("created detached raw-manifest proof assets: " + ", ".join(path.name for path in sidecars))
    return 0


def main(argv: list[str] | None = None) -> int:
    """Run component release packaging, validation, and proof commands."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    pack = commands.add_parser("pack", help="create a deterministic tar.gz payload archive")
    pack.add_argument("--payload-root", type=Path, required=True)
    pack.add_argument("--output", type=Path, required=True)
    pack.set_defaults(handler=_pack_command)

    create = commands.add_parser("create", help="create a component release manifest")
    create.add_argument("--descriptor", type=Path, required=True)
    create.add_argument("--artifact", type=Path)
    create.add_argument("--payload-root", type=Path)
    create.add_argument("--output", type=Path, required=True)
    create.set_defaults(handler=_create_command)

    verify = commands.add_parser("verify", help="verify manifest, payload, catalog, release, run, and attestation pins")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--artifact", type=Path)
    verify.add_argument("--catalog", type=Path)
    verify.add_argument("--manifest-uri")
    verify.add_argument("--verify-release", action="store_true")
    verify.add_argument("--verify-run", action="store_true")
    verify.add_argument("--verify-attestation", action="store_true")
    verify.add_argument("--verify-attestation-only", action="store_true")
    verify.set_defaults(handler=_verify_command)

    index = commands.add_parser("index", help="create an immutable repository release index")
    index.add_argument("--manifest", type=Path, action="append", default=[])
    index.add_argument("--manifest-dir", type=Path)
    index.add_argument("--channel", choices=("stable", "preview"), required=True)
    index.add_argument("--source-ref", required=True)
    index.add_argument("--source-commit", required=True)
    index.add_argument("--run-id", required=True)
    index.add_argument("--run-attempt", type=int, required=True)
    index.add_argument("--workflow", required=True)
    index.add_argument("--attestation-uri")
    index.add_argument("--output", type=Path, required=True)
    index.set_defaults(handler=_index_command)

    native_subjects = commands.add_parser(
        "native-manifest-subjects", help="write exact raw native manifest attestation subjects"
    )
    native_subjects.add_argument("--manifest-dir", type=Path, required=True)
    native_subjects.add_argument("--output", type=Path, required=True)
    native_subjects.add_argument("--repository", required=True)
    native_subjects.add_argument("--channel", choices=("stable", "preview"), required=True)
    native_subjects.add_argument("--source-ref", required=True)
    native_subjects.add_argument("--source-commit", required=True)
    native_subjects.set_defaults(handler=_native_manifest_subjects_command)

    native_proofs = commands.add_parser(
        "native-manifest-proof-assets", help="verify and name detached raw native manifest proof bundles"
    )
    native_proofs.add_argument("--manifest-dir", type=Path, required=True)
    native_proofs.add_argument("--checksum-file", type=Path, required=True)
    native_proofs.add_argument("--bundle", type=Path, required=True)
    native_proofs.add_argument("--repository", required=True)
    native_proofs.add_argument("--channel", choices=("stable", "preview"), required=True)
    native_proofs.add_argument("--source-ref", required=True)
    native_proofs.add_argument("--source-commit", required=True)
    native_proofs.add_argument("--gh-executable", default="gh")
    native_proofs.set_defaults(handler=_native_manifest_attestation_assets_command)

    verify_index_parser = commands.add_parser(
        "verify-index", help="verify an index, catalog, release, run, and attestation pin"
    )
    verify_index_parser.add_argument("--index", type=Path, required=True)
    verify_index_parser.add_argument("--catalog", type=Path)
    verify_index_parser.add_argument("--verify-release", action="store_true")
    verify_index_parser.add_argument("--verify-run", action="store_true")
    verify_index_parser.add_argument("--verify-attestation", action="store_true")
    verify_index_parser.add_argument("--verify-attestation-only", action="store_true")
    verify_index_parser.set_defaults(handler=lambda args: _verify_index_command(args))

    arguments = parser.parse_args(argv)
    try:
        return arguments.handler(arguments)
    except (ComponentArtifactError, OSError, subprocess.SubprocessError) as error:
        print(f"component-artifacts: ERROR: {error}", file=sys.stderr)
        return 2


def _verify_index_command(arguments: argparse.Namespace) -> int:
    """Print the result of validating one component release index."""
    document = verify_index(
        arguments.index,
        verify_run=arguments.verify_run,
        verify_attestation=arguments.verify_attestation,
        verify_release=arguments.verify_release,
        verify_attestation_only=arguments.verify_attestation_only,
        catalog_path=arguments.catalog,
    )
    print(f"verified index {document['repository']} {document['channel']} {document['indexDigest']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
