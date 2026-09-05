#!/usr/bin/env python3
"""Build and verify every Python SDK declared by repository policy.

根据仓库策略构建并校验全部 Python SDK；本工具只生成发布候选物，不执行发布。
"""

from __future__ import annotations

import argparse
import email
import subprocess
import sys
import tarfile
import tomllib
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence

import yaml


@dataclass(frozen=True)
class PythonPackage:
    """One policy-owned Python distribution and its source directory."""

    name: str
    version: str
    source: Path


Runner = Callable[..., subprocess.CompletedProcess[str]]


def load_packages(repo_path: Path) -> tuple[PythonPackage, ...]:
    """Resolve the exact policy package set from direct SDK manifests."""

    policy_path = repo_path / "repository-policy.yaml"
    policy = yaml.safe_load(policy_path.read_text(encoding="utf-8")) or {}
    declared = policy.get("artifacts", {}).get("package_units", [])
    if not isinstance(declared, list) or not declared or not all(isinstance(item, str) and item for item in declared):
        raise ValueError("repository policy requires a non-empty artifacts.package_units list")
    if len(set(declared)) != len(declared):
        raise ValueError("repository policy contains duplicate Python package units")

    sdk_root = repo_path / "sdk" / "python"
    discovered: dict[str, PythonPackage] = {}
    for manifest_path in sorted(sdk_root.glob("*/pyproject.toml")):
        document = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        project = document.get("project") or {}
        build_system = document.get("build-system") or {}
        name = project.get("name")
        version = project.get("version")
        readme = project.get("readme")
        if not isinstance(name, str) or not name:
            raise ValueError(f"{manifest_path} has no project.name")
        if not isinstance(version, str) or not version:
            raise ValueError(f"{manifest_path} has no project.version")
        if not isinstance(readme, str) or not readme or not (manifest_path.parent / readme).is_file():
            raise ValueError(f"{manifest_path} must reference an existing project.readme")
        if not build_system.get("build-backend") or not build_system.get("requires"):
            raise ValueError(f"{manifest_path} has an incomplete build-system")
        if name in discovered:
            raise ValueError(f"duplicate Python distribution name: {name}")
        discovered[name] = PythonPackage(name=name, version=version, source=manifest_path.parent)

    declared_set = set(declared)
    discovered_set = set(discovered)
    if declared_set != discovered_set:
        missing = sorted(discovered_set - declared_set)
        unknown = sorted(declared_set - discovered_set)
        details = []
        if missing:
            details.append(f"SDK manifests missing from policy: {', '.join(missing)}")
        if unknown:
            details.append(f"policy packages without direct SDK manifests: {', '.join(unknown)}")
        raise ValueError("; ".join(details))
    return tuple(discovered[name] for name in declared)


def build_packages(
    packages: Sequence[PythonPackage],
    output_dir: Path,
    *,
    runner: Runner = subprocess.run,
) -> tuple[Path, ...]:
    """Build wheel and sdist candidates, then verify their embedded metadata."""

    output_dir.mkdir(parents=True, exist_ok=True)
    if any(output_dir.iterdir()):
        raise ValueError(f"release output directory must be empty: {output_dir}")

    for package in packages:
        runner(
            [
                sys.executable,
                "-m",
                "build",
                "--no-isolation",
                "--outdir",
                str(output_dir),
                str(package.source),
            ],
            check=True,
            text=True,
        )

    artifacts = tuple(sorted(path for path in output_dir.iterdir() if path.is_file()))
    expected_count = len(packages) * 2
    if len(artifacts) != expected_count:
        raise ValueError(f"expected {expected_count} release artifacts, found {len(artifacts)}")

    expected = {(package.name, package.version) for package in packages}
    wheels = {_distribution_metadata(path) for path in artifacts if path.suffix == ".whl"}
    source_archives = {_distribution_metadata(path) for path in artifacts if path.name.endswith(".tar.gz")}
    if wheels != expected or source_archives != expected:
        raise ValueError(
            "built artifact metadata does not exactly match repository policy: "
            f"wheels={sorted(wheels)}, source_archives={sorted(source_archives)}, expected={sorted(expected)}"
        )
    return artifacts


def _distribution_metadata(path: Path) -> tuple[str, str]:
    if path.suffix == ".whl":
        with zipfile.ZipFile(path) as archive:
            members = [name for name in archive.namelist() if name.endswith(".dist-info/METADATA")]
            if len(members) != 1:
                raise ValueError(f"wheel must contain exactly one METADATA file: {path.name}")
            payload = archive.read(members[0]).decode("utf-8")
    elif path.name.endswith(".tar.gz"):
        with tarfile.open(path, mode="r:gz") as archive:
            members = [
                member
                for member in archive.getmembers()
                if member.name.endswith("/PKG-INFO") and member.name.count("/") == 1
            ]
            if len(members) != 1:
                raise ValueError(f"sdist must contain exactly one PKG-INFO file: {path.name}")
            extracted = archive.extractfile(members[0])
            if extracted is None:
                raise ValueError(f"cannot read sdist metadata: {path.name}")
            payload = extracted.read().decode("utf-8")
    else:
        raise ValueError(f"unexpected release artifact: {path.name}")

    metadata = email.message_from_string(payload)
    name = metadata.get("Name")
    version = metadata.get("Version")
    if not name or not version:
        raise ValueError(f"distribution metadata lacks Name or Version: {path.name}")
    return name, version


def main() -> int:
    parser = argparse.ArgumentParser(description="Build policy-owned Cyrene Python SDKs")
    parser.add_argument("--repo-path", default=".", help="Path to the Platform repository")
    parser.add_argument("--output-dir", default="release-dist", help="Empty release artifact directory")
    args = parser.parse_args()

    repo_path = Path(args.repo_path).resolve()
    output_dir = Path(args.output_dir)
    if not output_dir.is_absolute():
        output_dir = repo_path / output_dir
    try:
        packages = load_packages(repo_path)
        artifacts = build_packages(packages, output_dir)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"[FAILED] {error}", file=sys.stderr)
        return 1

    print(f"[PASSED] Built and verified {len(packages)} Python SDKs ({len(artifacts)} artifacts)")
    for artifact in artifacts:
        print(artifact.relative_to(repo_path) if artifact.is_relative_to(repo_path) else artifact)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
