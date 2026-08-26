#!/usr/bin/env python3
"""Validate CYRENE advanced-service manifests without third-party packages."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


def _matches_type(value: Any, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    return True


def validate(value: Any, schema: dict[str, Any], location: str = "$") -> list[str]:
    errors: list[str] = []
    expected = schema.get("type")
    if expected and not _matches_type(value, expected):
        return [f"{location}: expected {expected}, got {type(value).__name__}"]

    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{location}: {value!r} is not one of {schema['enum']!r}")
    if isinstance(value, str) and "pattern" in schema and not re.fullmatch(schema["pattern"], value):
        errors.append(f"{location}: {value!r} does not match {schema['pattern']!r}")

    if isinstance(value, dict):
        properties = schema.get("properties", {})
        for name in schema.get("required", []):
            if name not in value:
                errors.append(f"{location}: missing required property {name!r}")
        if schema.get("additionalProperties") is False:
            for name in value.keys() - properties.keys():
                errors.append(f"{location}: unexpected property {name!r}")
        for name, item in value.items():
            if name in properties:
                errors.extend(validate(item, properties[name], f"{location}.{name}"))

    if isinstance(value, list):
        minimum = schema.get("minItems")
        if minimum is not None and len(value) < minimum:
            errors.append(f"{location}: expected at least {minimum} item(s)")
        if schema.get("uniqueItems") and len({json.dumps(item, sort_keys=True) for item in value}) != len(value):
            errors.append(f"{location}: items must be unique")
        item_schema = schema.get("items")
        if item_schema:
            for index, item in enumerate(value):
                errors.extend(validate(item, item_schema, f"{location}[{index}]"))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema", required=True, type=Path)
    parser.add_argument("manifests", nargs="+", type=Path)
    args = parser.parse_args()

    schema = json.loads(args.schema.read_text(encoding="utf-8"))
    failed = False
    seen_services: set[str] = set()
    for manifest_path in args.manifests:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        errors = validate(manifest, schema)
        service = manifest.get("service")
        if service in seen_services:
            errors.append(f"$.service: duplicate service {service!r}")
        if service:
            seen_services.add(service)
        for source_root in manifest.get("source_roots", []):
            if not (manifest_path.parent / source_root).exists():
                errors.append(f"$.source_roots: missing path {source_root!r}")
        if errors:
            failed = True
            print(f"FAIL {manifest_path}", file=sys.stderr)
            for error in errors:
                print(f"  - {error}", file=sys.stderr)
        else:
            print(f"OK   {manifest_path}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
