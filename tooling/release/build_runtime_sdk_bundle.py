#!/usr/bin/env python3
"""Build the strict-policy Runtime Maintenance wheel and its durable bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

from build_python_sdks import _distribution_metadata, build_packages, load_packages
from component_artifacts import _write_deterministic_tar_gz


PACKAGE_NAME = "cyrene-runtime-maintenance"
PACKAGE_VERSION = "0.1.0"
WHEEL_NAME = "cyrene_runtime_maintenance-0.1.0-py3-none-any.whl"


def build(repository: Path, output: Path) -> tuple[Path, Path, str]:
    packages = load_packages(repository)
    package = next((row for row in packages if row.name == PACKAGE_NAME), None)
    if package is None or package.version != PACKAGE_VERSION:
        raise ValueError("repository policy must declare Runtime Maintenance SDK 0.1.0")
    if output.exists() or output.is_symlink():
        raise ValueError(f"output directory must be absent: {output}")
    output.mkdir(parents=True)
    dist = output / "dist"
    dist.mkdir()
    build_packages(packages, dist)
    wheel = dist / WHEEL_NAME
    if wheel.is_symlink() or not wheel.is_file() or _distribution_metadata(wheel) != (PACKAGE_NAME, PACKAGE_VERSION):
        raise ValueError(f"strict SDK build did not produce the expected wheel: {WHEEL_NAME}")
    bundle_root = output / "payload"
    bundle_root.mkdir()
    bundled_wheel = bundle_root / WHEEL_NAME
    shutil.copyfile(wheel, bundled_wheel)
    wheel_digest = hashlib.sha256(bundled_wheel.read_bytes()).hexdigest()
    metadata = {
        "distribution": PACKAGE_NAME,
        "version": PACKAGE_VERSION,
        "wheel": WHEEL_NAME,
        "wheelSha256": "sha256:" + wheel_digest,
    }
    (bundle_root / "sdk-release.json").write_text(
        json.dumps(metadata, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    archive = output / "cyrene-runtime-maintenance-sdk-linux-ubuntu-24.04-x86_64-python-3.12-library.tar.gz"
    _write_deterministic_tar_gz(bundle_root, archive)
    context_root = output / "runtime-maintenance-wheel"
    context_root.mkdir()
    shutil.copyfile(wheel, context_root / WHEEL_NAME)
    return archive, context_root / WHEEL_NAME, "sha256:" + wheel_digest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        archive, wheel, digest = build(args.repository.resolve(), args.output.resolve())
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Runtime Maintenance SDK build failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps({"bundle": str(archive), "wheel": str(wheel), "wheelSha256": digest}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
