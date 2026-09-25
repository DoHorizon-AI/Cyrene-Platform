#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Validate repository package metadata against the architecture classification.

中文：依据架构分类验证仓库的软件包元数据。
"""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CONFIG = ROOT / "tooling/architecture/license-boundaries.toml"
LICENSES = ROOT / "LICENSES"


def load_config() -> dict[str, list[str]]:
    with CONFIG.open("rb") as handle:
        return tomllib.load(handle)


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--all-features"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def license_value(value: object) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and isinstance(value.get("text"), str):
        return value["text"]
    return None


def main() -> int:
    classes = load_config()
    public = set(classes["public_permissive"])
    core = set(classes["core_copyleft"])
    separate = set(classes["separate_decision"])
    expected = {name: "Apache-2.0" for name in public | separate}
    expected.update({name: "AGPL-3.0-only" for name in core})

    failures: list[str] = []
    for required in (
        LICENSES / "AGPL-3.0-only.txt",
        LICENSES / "Apache-2.0.txt",
        ROOT / "LICENSE",
        ROOT / "LICENSING.md",
    ):
        if not required.is_file():
            failures.append(f"missing license file: {required.relative_to(ROOT)}")

    data = cargo_metadata()
    workspace_ids = set(data["workspace_members"])
    packages = {package["id"]: package for package in data["packages"]}
    workspace = {packages[package_id]["name"]: packages[package_id] for package_id in workspace_ids}
    if set(workspace) != set(expected):
        failures.append("Cargo workspace package set differs from license classification")

    for name, package in sorted(workspace.items()):
        actual = package.get("license")
        if actual != expected.get(name):
            failures.append(f"{name}: expected {expected.get(name)!r}, found {actual!r}")

    for relative in (
        "sdk/python/cyrene_artifacts/pyproject.toml",
        "sdk/python/cyrene_preflight/pyproject.toml",
    ):
        path = ROOT / relative
        with path.open("rb") as handle:
            project = tomllib.load(handle).get("project", {})
        actual = license_value(project.get("license"))
        if actual != "Apache-2.0":
            failures.append(f"{relative}: expected Apache-2.0, found {actual!r}")

    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1

    print(
        f"license metadata: PASS ({len(public)} public, {len(core)} Core, "
        f"{len(separate)} separate-decision packages)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
