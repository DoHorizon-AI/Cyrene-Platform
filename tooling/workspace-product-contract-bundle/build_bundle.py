#!/usr/bin/env python3
"""Build a source-pinned, offline Product OpenAPI release bundle.

The builder follows the canonical TCK and copies only pinned Product contract
files plus their local reference closure.

模块职责：按 TCK 和不可变 Git 提交构建离线 Product 契约包。
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import posixpath
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import unquote, urlsplit

import yaml


PLATFORM_ROOT = Path(__file__).resolve().parents[2]
TCK_PATH = "contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv"
EXPECTED_REPOSITORIES = {
    "Cyrene-Catalyst": "https://github.com/DoHorizon-AI/Cyrene-Catalyst.git",
    "Cyrene-Echo": "https://github.com/DoHorizon-AI/Cyrene-Echo.git",
    "Cyrene-Exchange": "https://github.com/DoHorizon-AI/Cyrene-Exchange.git",
    "Cyrene-Navigator": "https://github.com/DoHorizon-AI/Cyrene-Navigator.git",
    "Cyrene-Reactor": "https://github.com/DoHorizon-AI/Cyrene-Reactor.git",
    "Cyrene-Yield": "https://github.com/DoHorizon-AI/Cyrene-Yield.git",
}
EXPECTED_OPENAPI_VERSION = "3.1.2"
EXPECTED_CONTRACT_VERSION = "1.0.0"
MAX_FILE_BYTES = 4 * 1024 * 1024
MAX_BUNDLE_BYTES = 32 * 1024 * 1024
MAX_BUNDLE_FILES = 512
HEX_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
PERCENT_ESCAPE_RE = re.compile(r"%(?![0-9a-fA-F]{2})")


class BundleError(Exception):
    """A fail-closed input or bundle validation error."""


class DuplicateKeyError(BundleError):
    """A YAML mapping contains an ambiguous duplicate key."""


class StrictSafeLoader(yaml.SafeLoader):
    pass


def _construct_unique_mapping(loader: StrictSafeLoader, node: yaml.MappingNode, deep: bool = False) -> dict[Any, Any]:
    loader.flatten_mapping(node)
    result: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        try:
            duplicate = key in result
        except TypeError as exc:
            raise BundleError("YAML mapping contains an unhashable key") from exc
        if duplicate:
            raise DuplicateKeyError(f"duplicate YAML mapping key: {key!r}")
        result[key] = loader.construct_object(value_node, deep=deep)
    return result


StrictSafeLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_unique_mapping)


@dataclass(frozen=True)
class GitTreeEntry:
    mode: bytes
    object_id: bytes


class GitSource:
    """A canonical Product Git repository pinned to one immutable commit."""

    def __init__(self, name: str, path: Path, commit: str):
        if name not in EXPECTED_REPOSITORIES:
            raise BundleError(f"unsupported Product repository: {name}")
        if not HEX_SHA_RE.fullmatch(commit):
            raise BundleError(f"{name}: commit must be a full lowercase 40-character SHA")
        if path.is_symlink() or not path.exists() or not path.is_dir():
            raise BundleError(f"{name}: source must be an existing, non-symlink Git directory")
        self.name = name
        self.path = path.resolve(strict=True)
        self.commit = commit
        self._git_root = self._find_repository_root()
        self._validate_repository_identity()
        self._validate_commit()
        self.tree = self._read_tree()

    def _git(self, *args: str, check: bool = True) -> bytes:
        result = subprocess.run(
            ["git", "-C", str(self.path), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if check and result.returncode != 0:
            detail = result.stderr.decode("utf-8", "replace").strip()
            raise BundleError(f"{self.name}: Git command failed ({args[0]}): {detail or 'unknown error'}")
        return result.stdout

    def _find_repository_root(self) -> Path:
        top = self._git("rev-parse", "--show-toplevel", check=False)
        if top:
            root = Path(top.decode("utf-8", "strict").strip()).resolve(strict=True)
            if root != self.path:
                raise BundleError(f"{self.name}: source path must be the repository root")
            dirty = self._git(
                "status", "--porcelain=v1", "--untracked-files=all", "--ignore-submodules=none"
            )
            if dirty:
                raise BundleError(f"{self.name}: checkout is dirty; use a clean checkout or bare Git object store")
            return root
        if self._git("rev-parse", "--is-bare-repository", check=False).strip() == b"true":
            return self.path
        raise BundleError(f"{self.name}: source is not a Git worktree or bare Git object store")

    def _validate_repository_identity(self) -> None:
        urls = self._git("config", "--get-all", "remote.origin.url", check=False).decode("utf-8", "strict").splitlines()
        expected = EXPECTED_REPOSITORIES[self.name]
        expected_without_suffix = expected[:-4] if expected.endswith(".git") else expected
        if len(urls) != 1 or urls[0] not in (expected, expected_without_suffix):
            raise BundleError(
                f"{self.name}: origin must be exactly {expected} or {expected_without_suffix}"
            )

    def _validate_commit(self) -> None:
        resolved = self._git("rev-parse", "--verify", f"{self.commit}^{{commit}}", check=False).decode(
            "ascii", "replace"
        ).strip()
        if resolved != self.commit:
            raise BundleError(f"{self.name}: pinned commit is unavailable as a commit object")
        if self._git("rev-parse", "--is-bare-repository", check=False).strip() != b"true":
            head = self._git("rev-parse", "--verify", "HEAD^{commit}").decode("ascii", "strict").strip()
            if head != self.commit:
                raise BundleError(f"{self.name}: clean checkout HEAD does not match the supplied commit")

    def _read_tree(self) -> dict[bytes, GitTreeEntry]:
        raw = self._git("ls-tree", "-r", "-z", self.commit)
        entries: dict[bytes, GitTreeEntry] = {}
        for record in raw.split(b"\0"):
            if not record:
                continue
            try:
                metadata, tree_path = record.split(b"\t", 1)
                mode, object_type, object_id = metadata.split(b" ", 2)
            except ValueError as exc:
                raise BundleError(f"{self.name}: malformed Git tree entry") from exc
            if object_type != b"blob":
                continue
            entries[tree_path] = GitTreeEntry(mode=mode, object_id=object_id)
        return entries

    def read_file(self, repo_path: str) -> bytes:
        """Read one regular blob from this repository's pinned Git tree.

        Args:
            repo_path: Bundle-root path including this repository's name.
        Returns:
            The exact blob bytes recorded by the pinned commit.
        Raises:
            BundleError: If the path is missing, outside this repository, or not a regular file.

        只读取提交对象中的文件字节，不使用可变工作区内容。
        """
        prefix = self.name + "/"
        if not repo_path.startswith(prefix):
            raise BundleError(f"{self.name}: file path is outside the named repository: {repo_path}")
        encoded_path = repo_path[len(prefix) :].encode("utf-8", "strict")
        entry = self.tree.get(encoded_path)
        if entry is None:
            raise BundleError(f"{self.name}: referenced file is absent from pinned commit: {repo_path}")
        if entry.mode not in (b"100644", b"100755"):
            raise BundleError(f"{self.name}: symlink or non-regular source is forbidden: {repo_path}")
        size_text = self._git("cat-file", "-s", entry.object_id.decode("ascii")).decode("ascii", "strict").strip()
        size = int(size_text)
        if size > MAX_FILE_BYTES:
            raise BundleError(f"{self.name}: file exceeds {MAX_FILE_BYTES} bytes: {repo_path}")
        return self._git("cat-file", "blob", entry.object_id.decode("ascii"))


def _parse_document(path: str, raw: bytes) -> Any:
    suffix = PurePosixPath(path).suffix.lower()
    try:
        text = raw.decode("utf-8", "strict")
        if suffix == ".json":
            def unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
                result: dict[str, Any] = {}
                for key, value in pairs:
                    if key in result:
                        raise BundleError(f"duplicate JSON object key in {path}: {key!r}")
                    result[key] = value
                return result

            return json.loads(text, object_pairs_hook=unique_json_object)
        return yaml.load(text, Loader=StrictSafeLoader)
    except BundleError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError, yaml.YAMLError) as exc:
        raise BundleError(f"cannot parse {path}: {exc}") from exc


def _validate_relative_path(path: str, *, label: str) -> str:
    if not path or "\x00" in path or "\\" in path or path.startswith("/") or re.match(r"^[A-Za-z]:", path):
        raise BundleError(f"{label}: path is not a relative POSIX path: {path!r}")
    normalized = posixpath.normpath(path)
    if normalized in ("", ".", "..") or normalized.startswith("../"):
        raise BundleError(f"{label}: path escapes its root: {path!r}")
    if normalized != path:
        raise BundleError(f"{label}: path must be normalized: {path!r}")
    return normalized


def _contract_root(main_spec: str) -> str:
    parts = PurePosixPath(main_spec).parts
    if len(parts) < 4 or parts[1:3] != ("contracts", "product") or parts[3] != "v1":
        raise BundleError(f"TCK contract must be under <repository>/contracts/product/v1: {main_spec}")
    return "/".join(parts[:4])


def _resolve_reference(current_path: str, ref: str, contract_root: str) -> str | None:
    """Resolve a local `$ref`, rejecting URLs and paths outside Product v1.

    Returns `None` for an in-document fragment and a normalized bundle path for
    a local file reference.
    """
    if not isinstance(ref, str) or not ref:
        raise BundleError(f"{current_path}: $ref must be a non-empty string")
    try:
        parsed = urlsplit(ref)
    except ValueError as exc:
        raise BundleError(f"{current_path}: malformed $ref: {ref!r}") from exc
    if parsed.scheme or parsed.netloc or parsed.query:
        raise BundleError(f"{current_path}: remote or query-bearing $ref is forbidden: {ref!r}")
    if not parsed.path:
        if ref.startswith("#"):
            return None
        raise BundleError(f"{current_path}: empty-path $ref is unsupported: {ref!r}")
    if PERCENT_ESCAPE_RE.search(parsed.path):
        raise BundleError(f"{current_path}: malformed percent escape in $ref: {ref!r}")
    if "\\" in parsed.path or "\x00" in parsed.path or parsed.path.startswith("/"):
        raise BundleError(f"{current_path}: absolute or malformed $ref path is forbidden: {ref!r}")
    try:
        decoded_path = unquote(parsed.path, errors="strict")
    except UnicodeDecodeError as exc:
        raise BundleError(f"{current_path}: invalid UTF-8 encoding in $ref: {ref!r}") from exc
    if "\\" in decoded_path or "\x00" in decoded_path or decoded_path.startswith("/"):
        raise BundleError(f"{current_path}: absolute or malformed encoded $ref path is forbidden: {ref!r}")
    resolved = posixpath.normpath(posixpath.join(posixpath.dirname(current_path), decoded_path))
    if resolved != contract_root and not resolved.startswith(contract_root + "/"):
        raise BundleError(f"{current_path}: $ref leaves the Product v1 contract root: {ref!r}")
    _validate_relative_path(resolved, label=f"{current_path} $ref")
    if PurePosixPath(resolved).suffix.lower() not in (".json", ".yaml", ".yml"):
        raise BundleError(f"{current_path}: unsupported referenced file type: {ref!r}")
    return resolved


def _find_references(document: Any, current_path: str, contract_root: str) -> set[str]:
    found: set[str] = set()
    stack = [document]
    visited: set[int] = set()
    node_count = 0
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            identity = id(value)
            if identity in visited:
                continue
            visited.add(identity)
            node_count += len(value)
            if node_count > 1_000_000:
                raise BundleError(f"{current_path}: document structure exceeds the scan limit")
            for key, child in value.items():
                if key == "$ref":
                    target = _resolve_reference(current_path, child, contract_root)
                    if target is not None:
                        found.add(target)
                stack.append(child)
        elif isinstance(value, list):
            identity = id(value)
            if identity in visited:
                continue
            visited.add(identity)
            node_count += len(value)
            if node_count > 1_000_000:
                raise BundleError(f"{current_path}: document structure exceeds the scan limit")
            stack.extend(value)
    return found


def _read_tck(platform_root: Path) -> tuple[bytes, list[dict[str, str]]]:
    """Read the canonical TCK blob and return its selected contract paths.

    The TCK is read from Platform `HEAD`, so a mutable worktree edit cannot
    silently change the operation-to-contract selection.
    """
    if platform_root.is_symlink() or not platform_root.is_dir():
        raise BundleError("Platform root must be an existing non-symlink directory")
    tree_entry = subprocess.run(
        ["git", "-C", str(platform_root), "ls-tree", "HEAD", "--", TCK_PATH],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if tree_entry.returncode != 0:
        raise BundleError("cannot inspect canonical TCK entry in Platform HEAD")
    metadata = tree_entry.stdout.decode("utf-8", "strict").strip().split("\t", 1)
    if len(metadata) != 2 or metadata[1] != TCK_PATH or not metadata[0].startswith("100644 blob "):
        raise BundleError("canonical TCK must be a regular file in Platform HEAD")
    result = subprocess.run(
        ["git", "-C", str(platform_root), "show", f"HEAD:{TCK_PATH}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise BundleError(f"cannot read canonical TCK from Platform HEAD: {detail or 'Git show failed'}")
    raw = result.stdout
    if len(raw) > MAX_FILE_BYTES:
        raise BundleError("canonical TCK exceeds the file size limit")
    try:
        text = raw.decode("utf-8", "strict")
        rows = list(csv.DictReader(text.splitlines(), delimiter="\t", strict=True))
    except (UnicodeDecodeError, csv.Error) as exc:
        raise BundleError(f"cannot parse canonical TCK: {exc}") from exc
    expected_header = ["owner", "wire_operation", "product_operation_id", "kind", "product_contract"]
    reader_header = next(csv.reader(text.splitlines()[:1], delimiter="\t"), [])
    if reader_header != expected_header or not rows:
        raise BundleError("canonical TCK has an unexpected header or no Product operations")
    seen_operations: set[str] = set()
    selected: set[str] = set()
    for row in rows:
        if set(row) != set(expected_header) or any(not (row.get(key) or "").strip() for key in expected_header):
            raise BundleError("canonical TCK contains an incomplete Product operation row")
        operation_id = row["wire_operation"].strip()
        if operation_id in seen_operations:
            raise BundleError(f"canonical TCK repeats wire operation: {operation_id}")
        seen_operations.add(operation_id)
        if row["kind"].strip() not in ("READ", "COMMAND"):
            raise BundleError(f"canonical TCK has an unsupported operation kind: {row['kind']}")
        path = _validate_relative_path(row["product_contract"].strip(), label="TCK contract")
        parts = PurePosixPath(path).parts
        if len(parts) < 5 or parts[0] not in EXPECTED_REPOSITORIES:
            raise BundleError(f"canonical TCK names an unsupported Product contract: {path}")
        owner = parts[0].removeprefix("Cyrene-").upper()
        if row["owner"].strip() != owner:
            raise BundleError(f"canonical TCK owner does not match contract path: {path}")
        _contract_root(path)
        if PurePosixPath(path).suffix.lower() not in (".yaml", ".yml", ".json"):
            raise BundleError(f"canonical TCK selected contract is not YAML or JSON: {path}")
        selected.add(path)
    selected_repositories = {PurePosixPath(path).parts[0] for path in selected}
    if selected_repositories != set(EXPECTED_REPOSITORIES):
        raise BundleError("canonical TCK must select documents from exactly the six Product repositories")
    return raw, sorted(selected)


def _collect_closure(source: GitSource, main_specs: list[str]) -> dict[str, bytes]:
    """Collect and validate one owner's selected specs and local ref closure."""
    files: dict[str, bytes] = {}
    pending = list(main_specs)
    roots = {spec: _contract_root(spec) for spec in main_specs}
    total_bytes = 0
    while pending:
        source_path = pending.pop()
        root = next((root for spec, root in roots.items() if source_path == spec or source_path.startswith(root + "/")), None)
        if root is None:
            raise BundleError(f"{source.name}: file escaped all selected Product v1 roots: {source_path}")
        if source_path in files:
            continue
        if len(files) >= MAX_BUNDLE_FILES:
            raise BundleError(f"{source.name}: bundle exceeds {MAX_BUNDLE_FILES} files")
        raw = source.read_file(source_path)
        total_bytes += len(raw)
        if total_bytes > MAX_BUNDLE_BYTES:
            raise BundleError(f"{source.name}: bundle exceeds {MAX_BUNDLE_BYTES} bytes")
        document = _parse_document(source_path, raw)
        if isinstance(document, dict) and "openapi" in document:
            if document.get("openapi") != EXPECTED_OPENAPI_VERSION:
                raise BundleError(
                    f"{source_path}: expected OpenAPI {EXPECTED_OPENAPI_VERSION}, got {document.get('openapi')!r}"
                )
            info = document.get("info")
            if not isinstance(info, dict) or info.get("version") != EXPECTED_CONTRACT_VERSION:
                actual = info.get("version") if isinstance(info, dict) else None
                raise BundleError(
                    f"{source_path}: expected Product contract version {EXPECTED_CONTRACT_VERSION}, got {actual!r}"
                )
        files[source_path] = raw
        pending.extend(sorted(_find_references(document, source_path, root), reverse=True))
    return files


def _validate_output(output: Path, forbidden_roots: list[Path]) -> tuple[Path, Path]:
    if output.exists() or output.is_symlink():
        raise BundleError(f"output already exists; refusing to overwrite: {output}")
    parent = output.parent
    if not parent.exists() or not parent.is_dir():
        raise BundleError("output parent directory must already exist")
    resolved_parent = parent.resolve(strict=True)
    resolved_output = resolved_parent / output.name
    if output.name in ("", ".", ".."):
        raise BundleError("output must name a new directory")
    for root in forbidden_roots:
        resolved_root = root.resolve(strict=True)
        try:
            resolved_output.relative_to(resolved_root)
        except ValueError:
            continue
        raise BundleError(f"output must be outside source repositories and the Platform checkout: {resolved_output}")
    return resolved_parent, resolved_output


def build_bundle(
    output: Path,
    source_paths: dict[str, Path],
    commits: dict[str, str],
    *,
    platform_root: Path = PLATFORM_ROOT,
) -> dict[str, Any]:
    """Build a new bundle directory and return its deterministic manifest.

    Inputs must name all six canonical repositories and their exact commit
    SHAs. The output is created atomically and is never overwritten.

    Args:
        output: Nonexistent output directory with an existing parent directory.
        source_paths: Repository name to clean checkout or bare object-store path.
        commits: Repository name to full lowercase commit SHA.
        platform_root: Platform Git checkout containing the canonical TCK at `HEAD`.
    Returns:
        The manifest dictionary written at the bundle root.
    Raises:
        BundleError: If any source, reference, version, or output constraint fails.

    仅按明确提交锁定的来源生成产物；不会访问网络或改写 Product 契约。
    """
    expected_names = set(EXPECTED_REPOSITORIES)
    if set(source_paths) != expected_names or set(commits) != expected_names:
        raise BundleError("exactly six --source and six --commit entries are required")
    tck_raw, main_specs = _read_tck(platform_root)
    source_map = {
        name: GitSource(name, source_paths[name], commits[name])
        for name in sorted(EXPECTED_REPOSITORIES)
    }
    output_parent, resolved_output = _validate_output(
        output,
        [platform_root, *(source.path for source in source_map.values())],
    )

    files: dict[str, bytes] = {}
    for repository in sorted(EXPECTED_REPOSITORIES):
        repo_specs = [path for path in main_specs if PurePosixPath(path).parts[0] == repository]
        files.update(_collect_closure(source_map[repository], repo_specs))
    if len(files) > MAX_BUNDLE_FILES:
        raise BundleError(f"bundle exceeds {MAX_BUNDLE_FILES} files")
    if sum(len(raw) for raw in files.values()) > MAX_BUNDLE_BYTES:
        raise BundleError(f"bundle exceeds {MAX_BUNDLE_BYTES} bytes")

    manifest = {
        "formatVersion": 1,
        "canonicalTckSha256": hashlib.sha256(tck_raw).hexdigest(),
        "repositories": [
            {"name": name, "commit": source_map[name].commit} for name in sorted(EXPECTED_REPOSITORIES)
        ],
        "files": [
            {"path": path, "sha256": hashlib.sha256(raw).hexdigest()} for path, raw in sorted(files.items())
        ],
    }
    manifest_raw = (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")

    staging = Path(tempfile.mkdtemp(prefix=".product-contract-bundle-", dir=output_parent))
    try:
        for relative_path, raw in sorted(files.items()):
            target = staging.joinpath(*PurePosixPath(relative_path).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(raw)
        (staging / "product-contract-bundle.json").write_bytes(manifest_raw)
        os.rename(staging, resolved_output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return manifest


def _parse_assignments(values: list[str], option_name: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for item in values:
        if "=" not in item:
            raise BundleError(f"{option_name} entries must be NAME=VALUE")
        name, value = item.split("=", 1)
        if not name or not value or name in result:
            raise BundleError(f"duplicate or empty {option_name} entry: {item!r}")
        result[name] = value
    return result


def main(argv: list[str] | None = None) -> int:
    """Run the command-line interface and emit a concise build result."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", action="append", required=True, metavar="NAME=GIT_PATH")
    parser.add_argument("--commit", action="append", required=True, metavar="NAME=40_HEX_SHA")
    parser.add_argument("--output", required=True, type=Path, help="new bundle directory; its parent must exist")
    args = parser.parse_args(argv)
    try:
        source_values = _parse_assignments(args.source, "--source")
        commit_values = _parse_assignments(args.commit, "--commit")
        manifest = build_bundle(
            args.output,
            {name: Path(value) for name, value in source_values.items()},
            commit_values,
        )
    except (BundleError, OSError, subprocess.SubprocessError) as exc:
        print(f"bundle build failed: {exc}", file=sys.stderr)
        return 2
    print(
        f"built Product contract bundle: {len(manifest['files'])} files, "
        f"TCK sha256 {manifest['canonicalTckSha256']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
