#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Reject normal/build dependency edges from public crates into Platform Core.

中文：禁止公开 crate 通过普通或构建依赖边依赖 Platform Core。
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CONFIG = ROOT / "tooling/architecture/license-boundaries.toml"

def load_config() -> dict[str, list[str]]:
    import tomllib

    with CONFIG.open("rb") as handle:
        data = tomllib.load(handle)
    return {key: list(value) for key, value in data.items() if isinstance(value, list)}


def metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--all-features"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def main() -> int:
    classes = load_config()
    public = set(classes["public_permissive"])
    core = set(classes["core_copyleft"])
    separate = set(classes["separate_decision"])
    if public & core or public & separate or core & separate:
        print("license-boundary classes overlap", file=sys.stderr)
        return 1

    data = metadata()
    packages = {package["id"]: package for package in data["packages"]}
    workspace_ids = set(data["workspace_members"])
    workspace_names = {packages[package_id]["name"] for package_id in workspace_ids}
    configured = public | core | separate
    missing = sorted(workspace_names ^ configured)
    if missing:
        print(f"unclassified workspace packages: {', '.join(missing)}", file=sys.stderr)
        return 1

    nodes = {node["id"]: node for node in data["resolve"]["nodes"]}
    id_by_name = {package["name"]: package_id for package_id, package in packages.items()}
    failures: list[str] = []
    for public_name in sorted(public):
        start = id_by_name[public_name]
        pending = [start]
        visited = {start}
        while pending:
            current = pending.pop()
            for dependency in nodes[current]["deps"]:
                kinds = {kind["kind"] for kind in dependency["dep_kinds"]}
                if kinds <= {"dev"}:
                    continue
                target = dependency["pkg"]
                target_name = packages[target]["name"]
                if target_name in core:
                    failures.append(f"{public_name} -> {target_name}")
                if target not in visited:
                    visited.add(target)
                    pending.append(target)

    if failures:
        print("public/core dependency boundary violation:", file=sys.stderr)
        for failure in sorted(set(failures)):
            print(f"  {failure}", file=sys.stderr)
        return 1

    package_by_name = {
        package["name"]: package for package in data["packages"] if package["id"] in workspace_ids
    }
    for public_name in sorted(public):
        source = Path(package_by_name[public_name]["manifest_path"]).parent
        for path in source.rglob("*.rs"):
            text = path.read_text(encoding="utf-8")
            for core_name in core:
                if core_name.replace("-", "_") in text:
                    print(
                        f"public crate source mentions core crate {core_name}: {path}",
                        file=sys.stderr,
                    )
                    failures.append(f"source:{public_name}:{core_name}")
    if failures:
        return 1

    print(f"license boundary: PASS ({len(public)} public crates; core closure excluded)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
