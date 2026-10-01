#!/usr/bin/env python3
"""Rebuild the tracked Product v2 contract context from its exact owner commits."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


PLATFORM_OWNER = "DoHorizon-AI"
SHA1 = re.compile(r"^[0-9a-f]{40}$")


def _run(command: list[str], *, cwd: Path | None = None) -> str:
    result = subprocess.run(command, cwd=cwd, check=True, capture_output=True, text=True)
    return result.stdout.strip()


def prepare(repository: Path, output: Path, checkout_root: Path) -> dict[str, Any]:
    lock_path = repository / "tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json"
    if lock_path.is_symlink() or not lock_path.is_file():
        raise ValueError(f"tracked Product contract lock is missing: {lock_path}")
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    if lock.get("wireApiVersion") != "cyrene.workspace.product.v2" or lock.get("formatVersion") != 2:
        raise ValueError("tracked Product contract lock has an unsupported wire/API version")
    owners = lock.get("owners")
    if not isinstance(owners, list) or not owners:
        raise ValueError("tracked Product contract lock has no owner pins")
    if output.exists() or output.is_symlink():
        raise ValueError(f"Product contract output must not exist: {output}")
    checkout_root.mkdir(parents=True, exist_ok=True)
    sources: list[tuple[str, Path, str]] = []
    for owner in owners:
        if not isinstance(owner, dict):
            raise ValueError("Product contract lock owner row must be an object")
        owner_id = owner.get("ownerId")
        repository_name = owner.get("repository")
        commit = owner.get("sourceSha")
        if not isinstance(owner_id, str) or not re.fullmatch(r"[a-z][a-z0-9-]{0,62}", owner_id):
            raise ValueError("Product contract lock has an invalid ownerId")
        if not isinstance(repository_name, str) or not re.fullmatch(r"Cyrene-[A-Za-z0-9-]{1,120}", repository_name):
            raise ValueError(f"{owner_id}: Product repository name is invalid")
        if not isinstance(commit, str) or not SHA1.fullmatch(commit):
            raise ValueError(f"{owner_id}: sourceSha must be a full immutable Git commit")
        checkout = checkout_root / repository_name
        if checkout.exists() or checkout.is_symlink():
            raise ValueError(f"refusing to reuse an existing Product checkout: {checkout}")
        checkout.mkdir(parents=True)
        origin = f"https://github.com/{PLATFORM_OWNER}/{repository_name}.git"
        _run(["git", "init", "--quiet"], cwd=checkout)
        _run(["git", "remote", "add", "origin", origin], cwd=checkout)
        _run(["git", "fetch", "--no-tags", "--depth=1", "origin", commit], cwd=checkout)
        _run(["git", "checkout", "--quiet", "--detach", "FETCH_HEAD"], cwd=checkout)
        actual = _run(["git", "rev-parse", "HEAD"], cwd=checkout)
        if actual != commit:
            raise ValueError(f"{owner_id}: fetched commit does not match tracked SHA")
        sources.append((owner_id, checkout, commit))

    command = [
        sys.executable,
        str(repository / "tooling/workspace-product-contract-bundle/build_v2_bundle.py"),
        "--output",
        str(output),
        "--policy",
        str(repository / "contracts/policies/workspace-product-policy-v2.json"),
        "--verify-lock",
        str(lock_path),
    ]
    for owner_id, checkout, commit in sources:
        command.extend(["--source", f"{owner_id}={checkout}", "--commit", f"{owner_id}={commit}"])
    _run(command, cwd=repository)
    return lock


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--checkout-root", type=Path, required=True)
    args = parser.parse_args()
    try:
        lock = prepare(args.repository.resolve(), args.output.resolve(), args.checkout_root.resolve())
    except (OSError, ValueError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"Product contract preparation failed: {error}", file=sys.stderr)
        return 2
    print(
        json.dumps(
            {
                "manifestSha256": lock["bundle"]["manifestSha256"],
                "owners": len(lock["owners"]),
                "output": str(args.output),
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
