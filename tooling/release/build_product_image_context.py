"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 build_product_image_context.py                                       │
│  Module: tooling.release.build_product_image_context                    │
│  Role: Materialize an OCI build context from exact repository revisions.  │
│                                                                          │
│  模块职责：只从固定 Git SHA 组合 Product OCI 构建上下文。                 │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any


PRODUCTS = {
    "catalyst": "Cyrene-Catalyst",
    "yield": "Cyrene-Yield",
    "reactor": "Cyrene-Reactor",
    "exchange": "Cyrene-Exchange",
    "echo": "Cyrene-Echo",
}
REPOSITORIES = {
    "Cyrene-Platform": "DoHorizon-AI/Cyrene-Platform",
    "Cyrene-Plugins-Official": "DoHorizon-AI/Cyrene-Plugins-Official",
    "Cyrene-Client": "DoHorizon-AI/Cyrene-Client",
    "Cyrene-Yield": "DoHorizon-AI/Cyrene-Yield",
}
IMAGE_INPUTS = {
    "catalyst": ("Cyrene-Platform", "Cyrene-Plugins-Official"),
    "yield": ("Cyrene-Platform", "Cyrene-Plugins-Official"),
    "reactor": ("Cyrene-Platform", "Cyrene-Plugins-Official", "Cyrene-Yield"),
    "exchange": ("Cyrene-Platform", "Cyrene-Plugins-Official", "Cyrene-Client"),
    "echo": ("Cyrene-Platform", "Cyrene-Plugins-Official"),
}
SHA1 = re.compile(r"^[0-9a-f]{40}$")


class ImageContextError(ValueError):
    """Raised when a release lock or archived source cannot be trusted."""


def _run(arguments: list[str], *, cwd: Path | None = None) -> str:
    """Run one Git command and preserve its concrete failure for CI logs."""
    try:
        result = subprocess.run(arguments, cwd=cwd, check=True, capture_output=True, text=True)
    except (OSError, subprocess.CalledProcessError) as error:
        raise ImageContextError(f"command failed: {' '.join(arguments)}: {error}") from error
    return result.stdout.strip()


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Reject duplicate JSON fields in the Workspace release lock."""
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ImageContextError(f"release lock contains a duplicate JSON key: {key!r}")
        value[key] = item
    return value


def _extract_archive(archive_path: Path, destination: Path) -> None:
    """Extract a Git archive while rejecting unsafe links and member paths."""
    destination.mkdir(parents=True, exist_ok=False)
    try:
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive.getmembers():
                relative = PurePosixPath(member.name)
                if (
                    not member.name
                    or "\\" in member.name
                    or relative.is_absolute()
                    or relative.as_posix() != member.name
                    or any(part in {"", ".", ".."} for part in relative.parts)
                ):
                    raise ImageContextError(f"Git archive has an unsafe path: {member.name!r}")
                if not (member.isfile() or member.isdir() or member.issym()):
                    raise ImageContextError(f"Git archive has an unsupported entry: {member.name!r}")
            archive.extractall(destination, filter="data")
    except (OSError, tarfile.TarError) as error:
        raise ImageContextError(f"cannot safely extract Git archive {archive_path}: {error}") from error


def _archive_commit(repository: str, commit: str, destination: Path, temporary_root: Path) -> None:
    """Fetch and unpack one exact Git object from its canonical public origin."""
    if repository not in REPOSITORIES or not SHA1.fullmatch(commit):
        raise ImageContextError(f"unsupported or unpinned image build source: {repository}@{commit}")
    source_repo = REPOSITORIES[repository]
    with tempfile.TemporaryDirectory(prefix="cyrene-image-source-", dir=temporary_root) as temporary:
        clone = Path(temporary) / "checkout"
        clone.mkdir()
        _run(["git", "init", "--quiet"], cwd=clone)
        _run(["git", "remote", "add", "origin", f"https://github.com/{source_repo}.git"], cwd=clone)
        _run(["git", "fetch", "--quiet", "--no-tags", "--depth", "1", "origin", commit], cwd=clone)
        actual_commit = _run(["git", "rev-parse", "FETCH_HEAD"], cwd=clone)
        if actual_commit != commit:
            raise ImageContextError(f"fetched {repository} object did not resolve to the requested SHA")
        archive_path = Path(temporary) / "source.tar"
        _run(["git", "archive", "--format=tar", f"--output={archive_path}", actual_commit], cwd=clone)
        _extract_archive(archive_path, destination)


def _archive_product(product_root: Path, product_name: str, product_commit: str, destination: Path) -> None:
    """Archive the clean Product checkout at its exact workflow source SHA."""
    if product_root.is_symlink() or not product_root.is_dir() or not SHA1.fullmatch(product_commit):
        raise ImageContextError("Product source root or commit is invalid")
    expected_remote = f"https://github.com/DoHorizon-AI/{product_name}.git"
    actual_remote = _run(["git", "remote", "get-url", "origin"], cwd=product_root)
    if actual_remote not in {expected_remote, expected_remote.removesuffix(".git")}:
        raise ImageContextError("Product source origin does not match the canonical repository")
    if _run(["git", "rev-parse", "HEAD"], cwd=product_root) != product_commit:
        raise ImageContextError("Product checkout does not match the workflow source SHA")
    if _run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=product_root):
        raise ImageContextError("Product checkout is dirty; refusing to build uncommitted source")
    destination.parent.mkdir(parents=True, exist_ok=True)
    archive_path = destination.parent / f"{product_name}.tar"
    _run(["git", "archive", "--format=tar", f"--output={archive_path}", product_commit], cwd=product_root)
    _extract_archive(archive_path, destination)


def build_context(
    *,
    product: str,
    product_root: Path,
    product_commit: str,
    workspace_root: Path,
    workspace_commit: str,
    output: Path,
) -> dict[str, Any]:
    """Create one standalone Docker context with only exact locked inputs.

    The Product is taken from the triggering source commit. Every sibling comes
    from the release-lock contained in the exact Workspace builder commit.
    """
    if product not in PRODUCTS:
        raise ImageContextError(f"unsupported Product image: {product}")
    if not SHA1.fullmatch(workspace_commit):
        raise ImageContextError("Workspace builder commit must be a full lowercase Git SHA")
    if output.exists() or output.is_symlink():
        raise ImageContextError(f"output directory must not exist: {output}")
    if workspace_root.is_symlink() or not workspace_root.is_dir():
        raise ImageContextError("Workspace builder checkout is missing or unsafe")
    if _run(["git", "rev-parse", "HEAD"], cwd=workspace_root) != workspace_commit:
        raise ImageContextError("Workspace builder checkout does not match its pinned SHA")
    if _run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=workspace_root):
        raise ImageContextError("Workspace builder checkout is dirty")

    lock_path = workspace_root / "release-lock.json"
    if lock_path.is_symlink() or not lock_path.is_file():
        raise ImageContextError("pinned Workspace release-lock.json is missing or unsafe")
    try:
        lock = json.loads(lock_path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object)
    except (OSError, json.JSONDecodeError) as error:
        raise ImageContextError(f"cannot read pinned Workspace release lock: {error}") from error
    pins = lock.get("repositories") if isinstance(lock, dict) else None
    if not isinstance(pins, dict):
        raise ImageContextError("Workspace release lock has no repositories map")

    root = output.resolve()
    product_repo_name = PRODUCTS[product]
    selected = list(IMAGE_INPUTS[product])
    if product_repo_name not in pins:
        raise ImageContextError(f"Workspace release lock has no Product pin for {product_repo_name}")
    commits: dict[str, str] = {}
    output.mkdir(parents=True)
    temporary_root = output.parent.resolve()
    product_destination = root / "Cyrene-Services" / product_repo_name
    _archive_product(product_root, product_repo_name, product_commit, product_destination)
    commits[product_repo_name] = product_commit

    for repository in selected:
        commit = pins.get(repository)
        if not isinstance(commit, str) or not SHA1.fullmatch(commit):
            raise ImageContextError(f"Workspace release lock has no exact SHA for {repository}")
        relative = Path(repository) if repository != "Cyrene-Yield" else Path("Cyrene-Services") / repository
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        _archive_commit(repository, commit, destination, temporary_root)
        commits[repository] = commit

    lock_digest = hashlib.sha256(lock_path.read_bytes()).hexdigest()
    dockerfile = f"Cyrene-Services/{product_repo_name}/Dockerfile"
    if not (root / dockerfile).is_file():
        raise ImageContextError(f"Product Dockerfile is absent from exact source: {dockerfile}")
    context_manifest = {
        "schemaVersion": 1,
        "product": product,
        "productCommit": product_commit,
        "workspaceBuilderCommit": workspace_commit,
        "releaseLockSha256": lock_digest,
        "repositories": dict(sorted(commits.items())),
        "dockerfile": dockerfile,
    }
    (root / "cyrene-build-context.json").write_text(
        json.dumps(context_manifest, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    return {"context": str(root), **context_manifest}


def main() -> int:
    """Parse and run the exact-source context builder."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", choices=tuple(PRODUCTS), required=True)
    parser.add_argument("--product-root", type=Path, required=True)
    parser.add_argument("--product-commit", required=True)
    parser.add_argument("--workspace-root", type=Path, required=True)
    parser.add_argument("--workspace-commit", required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = build_context(
            product=arguments.product,
            product_root=arguments.product_root.resolve(),
            product_commit=arguments.product_commit,
            workspace_root=arguments.workspace_root.resolve(),
            workspace_commit=arguments.workspace_commit,
            output=arguments.output.resolve(),
        )
    except (ImageContextError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"Product image context failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
