"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 product_contract_files.py                                       │
│  Module: tooling.release.product_contract_files                    │
│  Role: Collect the exact Product catalog and local OpenAPI refs.    │
│                                                                     │
│  模块职责：收集精确 Product catalog 与本地 OpenAPI 引用闭包。         │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import posixpath
import re
import stat
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import unquote, urlsplit

import yaml


class ProductContractFilesError(ValueError):
    """Raised when a Product catalog or referenced API document is unsafe."""


class _UniqueKeyLoader(yaml.SafeLoader):
    """Safe YAML loader that rejects duplicate mapping keys."""


def _construct_unique_mapping(loader: _UniqueKeyLoader, node: yaml.MappingNode, deep: bool = False) -> dict[Any, Any]:
    mapping: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        try:
            duplicate = key in mapping
        except TypeError as error:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                "mapping key is not hashable",
                key_node.start_mark,
            ) from error
        if duplicate:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"duplicate mapping key {key!r}",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


_UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_unique_mapping,
)


def _json_object_without_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Build one JSON object while rejecting duplicate member names."""
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def _safe_path(value: str) -> PurePosixPath:
    """Validate a normalized, repository-relative Product contract path."""
    path = PurePosixPath(value)
    if (
        not value
        or "\\" in value
        or "\x00" in value
        or path.is_absolute()
        or path.as_posix() != value
        or any(part in {"", ".", ".."} for part in path.parts)
    ):
        raise ProductContractFilesError(f"unsafe Product contract path: {value!r}")
    return path


def _read_regular(root: Path, relative: str) -> bytes:
    """Read one regular file beneath a repository root without following links."""
    relative_path = _safe_path(relative)
    try:
        if root.is_symlink():
            raise ProductContractFilesError("Product repository root must not be a symlink")
        resolved_root = root.resolve(strict=True)
        current = root
        for part in relative_path.parts[:-1]:
            current = current / part
            info = current.lstat()
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
                raise ProductContractFilesError(f"contract path traverses an unsafe directory: {relative}")
        path = current / relative_path.parts[-1]
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise ProductContractFilesError(f"contract input is not a regular file: {relative}")
        resolved_path = path.resolve(strict=True)
        resolved_path.relative_to(resolved_root)
        return resolved_path.read_bytes()
    except (OSError, ValueError) as error:
        if isinstance(error, ProductContractFilesError):
            raise
        raise ProductContractFilesError(f"cannot read Product contract file {relative}: {error}") from error


def _parse_api_document(path: str, raw: bytes) -> Any:
    """Decode JSON or YAML API data while rejecting duplicate mapping keys."""
    try:
        if path.endswith(".json"):
            return json.loads(
                raw.decode("utf-8", "strict"),
                object_pairs_hook=_json_object_without_duplicates,
            )
        text = raw.decode("utf-8", "strict")
        document = yaml.load(text, Loader=_UniqueKeyLoader)
    except (UnicodeDecodeError, json.JSONDecodeError, yaml.YAMLError, ValueError) as error:
        raise ProductContractFilesError(f"invalid Product OpenAPI document {path}: {error}") from error
    if not isinstance(document, (dict, list)):
        raise ProductContractFilesError(f"Product OpenAPI document must be an object or array: {path}")
    return document


def _local_references(document: Any) -> list[str]:
    references: list[str] = []
    stack = [document]
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            reference = value.get("$ref")
            if reference is not None:
                if not isinstance(reference, str) or not reference:
                    raise ProductContractFilesError("Product OpenAPI $ref must be a non-empty string")
                references.append(reference)
            stack.extend(value.values())
        elif isinstance(value, list):
            stack.extend(value)
    return references


def collect_product_contract_files(repository: Path, owner_id: str) -> dict[str, bytes]:
    """Return the Product v2 catalog and its full local OpenAPI reference closure.

    Args:
        repository: Exact Product repository checkout at its source commit.
        owner_id: Lowercase Product owner ID recorded in ``catalog.json``.
    Returns:
        A sorted path-to-bytes mapping suitable for insertion into a bundle.
    """
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,62}", owner_id):
        raise ProductContractFilesError(f"invalid Product owner ID: {owner_id!r}")
    root = Path(repository)
    catalog_path = "contracts/product/v2/catalog.json"
    catalog_raw = _read_regular(root, catalog_path)
    try:
        catalog = json.loads(
            catalog_raw.decode("utf-8", "strict"),
            object_pairs_hook=_json_object_without_duplicates,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise ProductContractFilesError(f"Product v2 catalog is invalid JSON: {error}") from error
    if not isinstance(catalog, dict) or catalog.get("ownerId") != owner_id:
        raise ProductContractFilesError("Product v2 catalog ownerId does not match the component")
    if catalog.get("schemaVersion") != "cyrene.product.operation-catalog.v2":
        raise ProductContractFilesError("Product v2 catalog schemaVersion is unsupported")
    operations = catalog.get("operations")
    if not isinstance(operations, list) or not operations:
        raise ProductContractFilesError("Product v2 catalog must have operations")

    files: dict[str, bytes] = {catalog_path: catalog_raw}
    pending: list[str] = []
    operation_documents: list[tuple[str, str]] = []
    seen_operations: set[str] = set()
    for operation in operations:
        if not isinstance(operation, dict):
            raise ProductContractFilesError("Product v2 catalog operation must be an object")
        operation_id = operation.get("operationId")
        route_id = operation.get("routeId")
        if (
            not isinstance(operation_id, str)
            or not re.fullmatch(r"[A-Za-z][A-Za-z0-9._-]{0,127}", operation_id)
            or route_id != operation_id
            or operation_id in seen_operations
        ):
            raise ProductContractFilesError(
                "Product v2 operationId/routeId identity is missing, duplicate, or malformed"
            )
        seen_operations.add(operation_id)
        openapi_path = operation.get("openapiPath")
        if not isinstance(openapi_path, str) or not re.fullmatch(
            r"contracts/product/v[1-9][0-9]*/[^?#]+\.ya?ml", openapi_path
        ):
            raise ProductContractFilesError("Product operation openapiPath is not a supported local contract path")
        pending.append(openapi_path)
        operation_documents.append((operation_id, openapi_path))

    visited: set[str] = set()
    parsed_documents: dict[str, Any] = {}
    while pending:
        current = pending.pop()
        _safe_path(current)
        match = re.match(r"^(contracts/product/v[1-9][0-9]*)/", current)
        if match is None:
            raise ProductContractFilesError(f"OpenAPI path is outside a Product contract version: {current}")
        version_root = match.group(1) + "/"
        if current in visited:
            continue
        visited.add(current)
        raw = files.get(current)
        if raw is None:
            raw = _read_regular(root, current)
            files[current] = raw
        document = _parse_api_document(current, raw)
        parsed_documents[current] = document
        for reference in _local_references(document):
            target, _separator, _fragment = reference.partition("#")
            if not target:
                continue
            parsed_target = urlsplit(target)
            if (
                parsed_target.scheme
                or parsed_target.netloc
                or target.startswith(("/", "data:"))
                or "\\" in target
                or "?" in target
                or re.search(r"%(?![0-9a-fA-F]{2})", target)
            ):
                raise ProductContractFilesError(f"remote or absolute OpenAPI reference is forbidden: {reference}")
            source_directory = posixpath.dirname(current)
            decoded_target = unquote(target, errors="strict")
            if "\x00" in decoded_target:
                raise ProductContractFilesError(f"invalid OpenAPI reference: {reference}")
            resolved = posixpath.normpath(posixpath.join(source_directory, decoded_target))
            _safe_path(resolved)
            if not resolved.startswith(version_root):
                raise ProductContractFilesError(f"OpenAPI reference leaves Product version root: {reference}")
            pending.append(resolved)

    for operation_id, openapi_path in operation_documents:
        document = parsed_documents.get(openapi_path)
        if document is None:
            raise ProductContractFilesError(f"Product OpenAPI operation document is missing: {openapi_path}")
        operation_ids: set[str] = set()
        stack = [document]
        while stack:
            value = stack.pop()
            if isinstance(value, dict):
                candidate = value.get("operationId")
                if isinstance(candidate, str):
                    operation_ids.add(candidate)
                stack.extend(value.values())
            elif isinstance(value, list):
                stack.extend(value)
        if operation_id not in operation_ids:
            raise ProductContractFilesError(
                f"Product catalog operationId is absent from its pinned OpenAPI document: {operation_id}"
            )

    return {path: files[path] for path in sorted(files)}
