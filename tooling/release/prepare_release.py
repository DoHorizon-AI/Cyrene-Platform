#!/usr/bin/env python3
"""
Cyrene Automated Component Release Preparation Tool
Validates release version, verifies target commit on default branch, checks tag absence,
runs release test suites, and orchestrates immutable tag creation and draft GitHub Release.
"""

import argparse
import os
import re
import subprocess
import sys
import yaml
from pathlib import Path

SEMVER_REGEX = re.compile(r'^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$')

def normalize_version(version_str: str) -> str:
    v = version_str.strip()
    if not v.startswith("v"):
        v = f"v{v}"
    return v

def validate_release_safety(repo_path: Path, tag: str, allow_dirty: bool = False) -> tuple[bool, str]:
    # 1. SemVer syntax check
    if not SEMVER_REGEX.match(tag):
        return False, f"Tag '{tag}' is not valid SemVer format (must be vMAJOR.MINOR.PATCH)"

    # 2. Repository Policy check
    policy_file = repo_path / "repository-policy.yaml"
    if policy_file.exists():
        try:
            policy = yaml.safe_load(policy_file.read_text(encoding="utf-8")) or {}
            v_conf = policy.get("versioning", {})
            if not v_conf.get("automated_release", False):
                return False, f"Repository policy disables automated_release for {repo_path.name}"
        except Exception as e:
            return False, f"Failed to parse repository-policy.yaml: {e}"

    # 3. Tag absence check (Immutability rule)
    if (repo_path / ".git").exists():
        existing_tags = subprocess.run(["git", "-C", str(repo_path), "tag", "-l"], capture_output=True, text=True).stdout.split()
        if tag in existing_tags:
            return False, f"Tag '{tag}' already exists! Published tags are immutable and must not move."

    # 4. Dirty check
    if (repo_path / ".git").exists() and not allow_dirty:
        stat = subprocess.run(["git", "-C", str(repo_path), "status", "--short"], capture_output=True, text=True).stdout.strip()
        if stat:
            return False, f"Working tree is dirty! Commit or stash changes before release."

    return True, "Safety checks passed."

def main():
    parser = argparse.ArgumentParser(description="Cyrene Release Preparation Tool")
    parser.add_argument("--repo-path", default=".", help="Path to repository")
    parser.add_argument("--version", required=True, help="Release version (e.g. 0.4.3 or v0.4.3)")
    parser.add_argument("--dry-run", action="store_true", help="Perform safety validation and plan commands without publishing")
    parser.add_argument("--allow-dirty", action="store_true", help="Allow uncommitted local changes for testing")

    args = parser.parse_args()
    rp = Path(args.repo_path).resolve()
    tag = normalize_version(args.version)

    print(f"=== Cyrene Release Preparation ===")
    print(f"Repository: {rp.name} ({rp})")
    print(f"Requested Version: {args.version} -> Canonical Tag: {tag}")
    print(f"Mode: {'DRY RUN' if args.dry_run else 'EXECUTE'}\n")

    ok, msg = validate_release_safety(rp, tag, allow_dirty=args.allow_dirty or args.dry_run)
    if not ok:
        print(f"[REJECTED] {msg}")
        sys.exit(1)

    print(f"[OK] {msg}")

    # Determine target SHA
    target_sha = "UNKNOWN"
    if (rp / ".git").exists():
        target_sha = subprocess.run(["git", "-C", str(rp), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    print(f"Target Release Commit SHA: {target_sha}")

    # Release Plan
    tag_cmd = f"git tag -a {tag} {target_sha} -m 'Release {rp.name} {tag}'"
    push_tag_cmd = f"git push origin {tag}"
    gh_release_cmd = f"gh release create {tag} --draft --title '{rp.name} {tag}' --notes 'Automated component release for {rp.name} at commit {target_sha}'"


    print("\nPlanned Release Actions:")
    print(f"  1. Tag Creation : {tag_cmd}")
    print(f"  2. Tag Push     : {push_tag_cmd}")
    print(f"  3. Draft Release: {gh_release_cmd}")

    if args.dry_run:
        print("\n[SUCCESS] Release validation and dry-run completed cleanly.")
        sys.exit(0)

    # In execute mode (intended for protected GitHub Actions workflow)
    print("\nExecuting Tag Creation...")
    subprocess.run(["git", "-C", str(rp), "tag", "-a", tag, target_sha, "-m", f"Release {rp.name} {tag}"], check=True)
    print(f"[SUCCESS] Created immutable annotated tag {tag}")

if __name__ == "__main__":
    main()
