"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 validate_component_schema.py                                    │
│  Module: tooling.release.validate_component_schema                 │
│  Role: Validate component documents against the pinned Workspace    │
│        JSON Schemas without network schema resolution.              │
│                                                                     │
│  模块职责：使用固定 Workspace JSON Schema 校验组件文档，禁止联网解析。 │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator
from referencing import Registry, Resource


SCHEMAS = {
    "manifest": "component-release-manifest-v1.schema.json",
    "index": "component-release-index-v1.schema.json",
}
V2_MANIFEST_SCHEMA = "component-release-manifest-v2.schema.json"


class ComponentSchemaError(ValueError):
    """Raised when a component document or pinned schema is invalid."""


def validate_document(document_path: Path, schema_dir: Path, kind: str) -> list[str]:
    """Validate one JSON document against an immutable Workspace schema set.

    Args:
        document_path: Manifest or index JSON file to validate.
        schema_dir: Directory containing both pinned release schema files.
        kind: Either ``manifest`` or ``index``.
    Returns:
        A sorted list of human-readable JSON Schema violations.
    """
    schema_name = SCHEMAS[kind]
    paths = {name: schema_dir / name for name in {*SCHEMAS.values(), V2_MANIFEST_SCHEMA}}
    schemas: dict[str, dict[str, Any]] = {}
    for name, path in paths.items():
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ComponentSchemaError(f"cannot load pinned schema {path}: {error}") from error
        if not isinstance(value, dict) or not isinstance(value.get("$id"), str):
            raise ComponentSchemaError(f"pinned schema must be a JSON object with $id: {path}")
        Draft202012Validator.check_schema(value)
        schemas[name] = value

    try:
        document = json.loads(document_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ComponentSchemaError(f"cannot load component JSON {document_path}: {error}") from error

    registry = Registry()
    for schema in schemas.values():
        registry = registry.with_resource(schema["$id"], Resource.from_contents(schema))
    selected_schema = schemas[schema_name]
    if kind == "manifest" and isinstance(document, dict) and document.get("schemaVersion") == 2:
        selected_schema = schemas[V2_MANIFEST_SCHEMA]
    validator = Draft202012Validator(selected_schema, registry=registry)
    failures = sorted(
        validator.iter_errors(document),
        key=lambda error: (list(error.absolute_path), error.message),
    )
    return [
        f"{document_path}:{'/'.join(str(part) for part in error.absolute_path) or '<root>'}: {error.message}"
        for error in failures
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=tuple(SCHEMAS), required=True)
    parser.add_argument("--document", type=Path, required=True)
    parser.add_argument("--schema-dir", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        failures = validate_document(arguments.document.resolve(), arguments.schema_dir.resolve(), arguments.kind)
    except (ComponentSchemaError, OSError, ValueError) as error:
        print(f"component schema validation failed: {error}", file=sys.stderr)
        return 2
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"schema-valid {arguments.kind}: {arguments.document}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
