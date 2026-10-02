#!/usr/bin/env python3
"""Build and verify dynamic Workspace Product v2 release bundles."""

from __future__ import annotations

import argparse
import hashlib
import json
import posixpath
import re
import subprocess
import sys
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import unquote

import yaml


PLATFORM_ROOT = Path(__file__).resolve().parents[2]
POLICY_SOURCE = PLATFORM_ROOT / "contracts/policies/workspace-product-policy-v2.json"
POLICY_BUNDLE_PATH = "workspace-product-policy-v2.json"
MANIFEST_NAME = "product-contract-bundle.json"
WIRE_API_VERSION = "cyrene.workspace.product.v2"
POLICY_SCHEMA_VERSION = "cyrene.workspace.product.authorization-policy.v2"
CONTRACT_API_VERSION = tomllib.loads(
    (PLATFORM_ROOT / "framework/crates/cy-workspace-product-contracts/Cargo.toml").read_text(
        encoding="utf-8"
    )
)["package"]["version"]
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_BUNDLE_BYTES = 32 * 1024 * 1024
MAX_FILES = 512
SHA1 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
INVALID_PERCENT_ESCAPE = re.compile(r"%(?![0-9a-fA-F]{2})")
OWNER = re.compile(r"^[a-z][a-z0-9-]{0,62}$")
REPOSITORY = re.compile(r"^Cyrene-[A-Za-z0-9-]{1,120}$")
OPENAPI_PATH = re.compile(r"^contracts/product/v[1-9][0-9]*/[^?#]+\.ya?ml$")


class BundleError(Exception):
    """A fail-closed input or bundle validation error."""


class StrictSafeLoader(yaml.SafeLoader):
    pass


def _construct_unique_mapping(loader: StrictSafeLoader, node: yaml.MappingNode, deep: bool = False) -> dict[Any, Any]:
    loader.flatten_mapping(node)
    result: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if not isinstance(key, (str, int, float, bool, type(None))):
            raise BundleError("YAML mapping contains an unsupported key")
        if key in result:
            raise BundleError(f"YAML mapping contains duplicate key: {key!r}")
        result[key] = loader.construct_object(value_node, deep=deep)
    return result


StrictSafeLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_unique_mapping)


def parse_document(path: str, raw: bytes) -> Any:
    try:
        text = raw.decode("utf-8", "strict")
        if path.endswith(".json"):
            def unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
                result: dict[str, Any] = {}
                for key, value in pairs:
                    if key in result:
                        raise BundleError(f"duplicate JSON object key in {path}: {key!r}")
                    result[key] = value
                return result
            return json.loads(text, object_pairs_hook=unique_json_object)
        if path.endswith((".yaml", ".yml")):
            return yaml.load(text, Loader=StrictSafeLoader)
    except BundleError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError, yaml.YAMLError) as exc:
        raise BundleError(f"cannot parse {path}: {exc}") from exc
    raise BundleError(f"unsupported contract file type: {path}")


class PinnedGitOwner:
    """Reads only regular file blobs from one exact owner Git commit."""

    def __init__(self, owner_id: str, path: Path, commit: str):
        if not OWNER.fullmatch(owner_id) or not REPOSITORY.fullmatch(path.name):
            raise BundleError("owner ID or Product repository name is invalid")
        if not SHA1.fullmatch(commit):
            raise BundleError(f"{owner_id}: source commit must be a full lowercase SHA")
        if path.is_symlink() or not path.is_dir():
            raise BundleError(f"{owner_id}: source must be a repository directory")
        self.owner_id = owner_id
        self.repository = path.name
        self.path = path.resolve(strict=True)
        self.commit = commit
        git_root = Path(self._git("rev-parse", "--show-toplevel").decode("utf-8", "strict").strip())
        if git_root.resolve(strict=True) != self.path:
            raise BundleError(f"{owner_id}: source path must be repository root")
        remotes = self._git("config", "--get-all", "remote.origin.url", check=False).decode("utf-8", "strict").splitlines()
        expected = f"https://github.com/DoHorizon-AI/{self.repository}.git"
        if remotes != [expected] and remotes != [expected.removesuffix(".git")]:
            raise BundleError(f"{owner_id}: origin must be exactly {expected}")
        resolved = self._git("rev-parse", "--verify", f"{commit}^{{commit}}", check=False).decode("ascii", "replace").strip()
        if resolved != commit:
            raise BundleError(f"{owner_id}: pinned commit is unavailable as a commit object")
        self.tree = self._read_tree()

    def _git(self, *args: str, check: bool = True) -> bytes:
        result = subprocess.run(
            ["git", "-C", str(self.path), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if check and result.returncode != 0:
            message = result.stderr.decode("utf-8", "replace").strip()
            raise BundleError(f"{self.owner_id}: Git command failed: {message or args[0]}")
        return result.stdout

    def _read_tree(self) -> dict[str, tuple[bytes, bytes]]:
        result = self._git("ls-tree", "-r", "-z", self.commit)
        tree: dict[str, tuple[bytes, bytes]] = {}
        for record in result.split(b"\0"):
            if not record:
                continue
            metadata, raw_path = record.split(b"\t", 1)
            mode, object_type, object_id = metadata.split(b" ", 2)
            if object_type == b"blob":
                tree[raw_path.decode("utf-8", "strict")] = (mode, object_id)
        return tree

    def read(self, path: str) -> bytes:
        prefix = f"{self.repository}/"
        if not path.startswith(prefix):
            raise BundleError(f"{self.owner_id}: path is outside its repository")
        relative = path[len(prefix):]
        record = self.tree.get(relative)
        if record is None:
            raise BundleError(f"{self.owner_id}: file is absent from the pinned commit: {path}")
        mode, object_id = record
        if mode not in (b"100644", b"100755"):
            raise BundleError(f"{self.owner_id}: symlink or non-regular file is forbidden: {path}")
        identifier = object_id.decode("ascii")
        size = int(self._git("cat-file", "-s", identifier).decode("ascii"))
        if size > MAX_FILE_BYTES:
            raise BundleError(f"{self.owner_id}: file exceeds size limit: {path}")
        return self._git("cat-file", "blob", identifier)


def validate_catalog(owner: PinnedGitOwner, path: str, raw: bytes) -> dict[str, Any]:
    catalog = parse_document(path, raw)
    if not isinstance(catalog, dict):
        raise BundleError(f"{owner.owner_id}: catalog root must be an object")
    if catalog.get("schemaVersion") != "cyrene.product.operation-catalog.v2":
        raise BundleError(f"{owner.owner_id}: unsupported catalog schema version")
    if catalog.get("ownerId") != owner.owner_id or not isinstance(catalog.get("catalogVersion"), str):
        raise BundleError(f"{owner.owner_id}: catalog identity or version is invalid")
    operations = catalog.get("operations")
    if not isinstance(operations, list) or not operations:
        raise BundleError(f"{owner.owner_id}: catalog operations must be a non-empty array")
    seen: set[str] = set()
    for operation in operations:
        if not isinstance(operation, dict):
            raise BundleError(f"{owner.owner_id}: operation must be an object")
        operation_id = operation.get("operationId")
        if not isinstance(operation_id, str) or not operation_id or operation_id in seen:
            raise BundleError(f"{owner.owner_id}: operation ID is missing or duplicated")
        seen.add(operation_id)
        if operation.get("routeId") != operation_id:
            raise BundleError(f"{owner.owner_id}/{operation_id}: routeId must equal operationId")
        if not isinstance(operation.get("openapiPath"), str) or not OPENAPI_PATH.fullmatch(operation["openapiPath"]):
            raise BundleError(f"{owner.owner_id}/{operation_id}: invalid openapiPath")
        if operation.get("kind") not in ("READ", "COMMAND"):
            raise BundleError(f"{owner.owner_id}/{operation_id}: invalid operation kind")
        for field in ("resourceId", "idempotency", "scope"):
            if not isinstance(operation.get(field), dict):
                raise BundleError(f"{owner.owner_id}/{operation_id}: {field} must be an object")
        pointers = operation.get("responseSchemaPointers")
        if not isinstance(pointers, list) or not all(isinstance(pointer, str) for pointer in pointers):
            raise BundleError(f"{owner.owner_id}/{operation_id}: response schema pointers are invalid")
        if operation.get("requestSchemaPointer") is not None and not isinstance(operation.get("requestSchemaPointer"), str):
            raise BundleError(f"{owner.owner_id}/{operation_id}: request schema pointer is invalid")
    return catalog


def normalize_repo_path(path: str) -> str:
    if not path or "\x00" in path or "\\" in path or path.startswith("/") or re.match(r"^[A-Za-z]:", path):
        raise BundleError(f"contract path is not relative POSIX: {path!r}")
    normalized = posixpath.normpath(path)
    if normalized in ("", ".", "..") or normalized.startswith("../") or normalized != path:
        raise BundleError(f"contract path escapes or is not normalized: {path!r}")
    return normalized


def add_reference_closure(
    owner: PinnedGitOwner,
    current_path: str,
    document: Any,
    files: dict[str, bytes],
) -> None:
    pending: list[tuple[str, Any]] = [(current_path, document)]
    visited: set[str] = set()
    while pending:
        source_path, source_document = pending.pop()
        if source_path in visited:
            continue
        visited.add(source_path)
        stack = [source_document]
        while stack:
            value = stack.pop()
            if isinstance(value, dict):
                reference = value.get("$ref")
                if isinstance(reference, str):
                    target, _, _fragment = reference.partition("#")
                    if (
                        "://" in target
                        or target.startswith("/")
                        or target.startswith("data:")
                        or "\\" in target
                        or "?" in target
                        or INVALID_PERCENT_ESCAPE.search(target)
                    ):
                        raise BundleError(f"remote or absolute reference is forbidden: {reference}")
                    if target:
                        parts = source_path.split("/")
                        if len(parts) < 5 or parts[1:3] != ["contracts", "product"]:
                            raise BundleError(f"reference source is outside a Product contract: {source_path}")
                        contract_prefix = "/".join(parts[:4]) + "/"
                        source_directory = posixpath.dirname(source_path)
                        resolved = normalize_repo_path(posixpath.normpath(posixpath.join(source_directory, unquote(target))))
                        if not resolved.startswith(contract_prefix):
                            raise BundleError(f"reference leaves Product contract version root: {reference}")
                        if resolved not in files:
                            raw = owner.read(resolved)
                            files[resolved] = raw
                            nested = parse_document(resolved, raw)
                            pending.append((resolved, nested))
                stack.extend(value.values())
            elif isinstance(value, list):
                stack.extend(value)


def build_release(
    output: Path,
    sources: dict[str, Path],
    commits: dict[str, str],
    policy_path: Path,
    *,
    lock_output: Path | None = None,
    verify_lock: Path | None = None,
) -> dict[str, Any]:
    if set(sources) != set(commits) or not sources:
        raise BundleError("each source owner must have exactly one commit pin")
    try:
        output_parent = output.parent.resolve(strict=True)
    except OSError as exc:
        raise BundleError("output parent must already exist") from exc
    if not output_parent.is_dir():
        raise BundleError("output parent must be a directory")
    output = output_parent / output.name
    if output.exists() or output.is_symlink():
        raise BundleError("output directory already exists")
    if lock_output and verify_lock:
        raise BundleError("choose either --write-lock or --verify-lock")
    if policy_path.is_symlink() or not policy_path.is_file():
        raise BundleError("Platform authorization policy must be a regular file")
    if policy_path.stat().st_size > 1024 * 1024:
        raise BundleError("Platform authorization policy exceeds size limit")
    policy_raw = policy_path.read_bytes()
    policy = parse_document(policy_path.name, policy_raw)
    if not isinstance(policy, dict) or policy.get("schemaVersion") != POLICY_SCHEMA_VERSION:
        raise BundleError("Platform authorization policy schema version is invalid")

    owners: list[dict[str, str]] = []
    artifacts: dict[str, bytes] = {}
    parsed_owners: list[tuple[PinnedGitOwner, dict[str, Any]]] = []
    for owner_id, path in sorted(sources.items()):
        owner = PinnedGitOwner(owner_id, path, commits[owner_id])
        catalog_path = f"{owner.repository}/contracts/product/v2/catalog.json"
        catalog_raw = owner.read(catalog_path)
        catalog = validate_catalog(owner, catalog_path, catalog_raw)
        if any(operation["openapiPath"].startswith("../") for operation in catalog["operations"]):
            raise BundleError(f"{owner_id}: OpenAPI path escapes repository")
        artifacts[catalog_path] = catalog_raw
        parsed_owners.append((owner, catalog))
        owners.append({
            "ownerId": owner_id,
            "repository": owner.repository,
            "sourceSha": owner.commit,
            "catalogPath": catalog_path,
            "catalogSha256": hashlib.sha256(catalog_raw).hexdigest(),
        })

    for owner, catalog in parsed_owners:
        for operation in catalog["operations"]:
            document_path = f"{owner.repository}/{operation['openapiPath']}"
            if document_path not in artifacts:
                raw = owner.read(document_path)
                artifacts[document_path] = raw
                document = parse_document(document_path, raw)
                add_reference_closure(owner, document_path, document, artifacts)

    artifacts[POLICY_BUNDLE_PATH] = policy_raw
    if len(artifacts) > MAX_FILES or sum(len(value) for value in artifacts.values()) > MAX_BUNDLE_BYTES:
        raise BundleError("bundle exceeds file count or total byte limits")
    files = [
        {"path": path, "sha256": hashlib.sha256(raw).hexdigest()}
        for path, raw in sorted(artifacts.items())
    ]
    manifest = {
        "formatVersion": 2,
        "wireApiVersion": WIRE_API_VERSION,
        "owners": owners,
        "files": files,
    }
    manifest_raw = (json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()
    manifest_sha = hashlib.sha256(manifest_raw).hexdigest()
    expected_lock = {
        "formatVersion": 2,
        "releaseId": "workspace-product-v2-initial",
        "wireApiVersion": WIRE_API_VERSION,
        "contractApiVersion": CONTRACT_API_VERSION,
        "bundle": {
            "manifestPath": MANIFEST_NAME,
            "manifestSha256": manifest_sha,
        },
        "policy": {
            "sourcePath": str(policy_path.resolve().relative_to(PLATFORM_ROOT))
            if policy_path.resolve().is_relative_to(PLATFORM_ROOT)
            else policy_path.name,
            "bundlePath": POLICY_BUNDLE_PATH,
            "schemaVersion": POLICY_SCHEMA_VERSION,
            "sha256": hashlib.sha256(policy_raw).hexdigest(),
        },
        "owners": owners,
    }

    if verify_lock:
        lock = parse_document(str(verify_lock), verify_lock.read_bytes())
        if lock != expected_lock:
            raise BundleError("generated bundle does not exactly match the trusted release lock")

    output.mkdir()
    (output / MANIFEST_NAME).write_bytes(manifest_raw)
    for path, raw in sorted(artifacts.items()):
        target = output / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
    if lock_output:
        lock_output.parent.mkdir(parents=True, exist_ok=True)
        lock_output.write_text(json.dumps(expected_lock, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return expected_lock


def parse_pairs(values: list[str], label: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for value in values:
        key, separator, item = value.partition("=")
        if not separator or not key or not item or key in result:
            raise BundleError(f"{label} entries must be unique OWNER=VALUE pairs")
        result[key] = item
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", action="append", default=[], metavar="OWNER=PATH")
    parser.add_argument("--commit", action="append", default=[], metavar="OWNER=SHA")
    parser.add_argument("--policy", type=Path, default=POLICY_SOURCE)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--write-lock", type=Path)
    parser.add_argument("--verify-lock", type=Path)
    args = parser.parse_args(argv)
    try:
        lock = build_release(
            args.output,
            {owner: Path(path) for owner, path in parse_pairs(args.source, "source").items()},
            parse_pairs(args.commit, "commit"),
            args.policy,
            lock_output=args.write_lock,
            verify_lock=args.verify_lock,
        )
        print(json.dumps({
            "manifestSha256": lock["bundle"]["manifestSha256"],
            "ownerCount": len(lock["owners"]),
            "fileCount": len(json.loads((args.output / MANIFEST_NAME).read_text())["files"]),
            "output": str(args.output),
        }, sort_keys=True))
        return 0
    except (BundleError, OSError) as exc:
        print(f"workspace Product v2 bundle build failed: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
