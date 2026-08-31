#!/usr/bin/env python3
"""
Cyrene Multi-Repository Workspace Orchestrator
Provides status, doctor, bootstrap/hydration, and dependency closure management.
"""

import argparse
import os
import shutil
import subprocess
import sys
import yaml
from pathlib import Path

def find_platform_root() -> Path:
    curr = Path.cwd().resolve()
    for parent in [curr] + list(curr.parents):
        if (parent / "tooling" / "workspace" / "repos.yaml").is_file():
            return parent
        candidate = parent / "Cyrene-Platform"
        if (candidate / "tooling" / "workspace" / "repos.yaml").is_file():
            return candidate
    return curr


def find_workspace_root(platform_root: Path) -> Path:
    return platform_root.parent


def load_catalog(platform_root: Path) -> dict:
    cat_file = platform_root / "tooling" / "workspace" / "repos.yaml"
    if not cat_file.exists():
        return {"profiles": {}, "repositories": {}}
    try:
        return yaml.safe_load(cat_file.read_text(encoding="utf-8")) or {}
    except Exception:
        return {"profiles": {}, "repositories": {}}

def load_baseline(platform_root: Path) -> dict:
    base_file = platform_root / "tooling" / "workspace" / "workspace-baseline.yaml"
    if not base_file.exists():
        return {}
    try:
        data = yaml.safe_load(base_file.read_text(encoding="utf-8")) or {}
        return data.get("repositories", {})
    except Exception:
        return {}

def resolve_dependency_closure(catalog: dict, target_repos: list) -> set:
    repos_meta = catalog.get("repositories", {})
    closure = set()
    stack = list(target_repos)

    while stack:
        r_key = stack.pop()
        if r_key not in closure:
            closure.add(r_key)
            deps = repos_meta.get(r_key, {}).get("dependencies", [])
            for d in deps:
                if d not in closure:
                    stack.append(d)

    return closure

def cmd_status(args):
    platform_root = find_platform_root()
    root = find_workspace_root(platform_root)
    catalog = load_catalog(platform_root)
    baseline = load_baseline(platform_root)
    repos = catalog.get("repositories", {})

    print(f"=== Cyrene Workspace Status ===")
    print(f"Workspace Root: {root}\n")
    header = f"{'Repository':<20} | {'Present':<7} | {'Host':<10} | {'Visibility':<8} | {'Branch':<26} | {'Dirty':<5} | {'Baseline Ref':<10}"
    print(header)
    print("-" * len(header))

    for key, info in repos.items():
        name = info.get("logical_name", key)
        rel_path = info.get("canonical_path", key)
        target_path = platform_root if key == "platform" else root / rel_path
        present = target_path.exists()
        host = info.get("source_host", "unknown")
        vis = info.get("visibility", "public")
        base_ref = baseline.get(key, {}).get("ref", "HEAD")[:7]

        branch = "MISSING"
        dirty = "N/A"
        if present:
            try:
                b = subprocess.run(["git", "-C", str(target_path), "branch", "--show-current"], capture_output=True, text=True).stdout.strip()
                branch = b if b else "DETACHED"
                stat = subprocess.run(["git", "-C", str(target_path), "status", "--short"], capture_output=True, text=True).stdout.strip()
                dirty = "DIRTY" if stat else "CLEAN"
            except Exception:
                branch = "ERROR"

        print(f"{name:<20} | {str(present):<7} | {host:<10} | {vis:<8} | {branch:<26} | {dirty:<5} | {base_ref:<10}")

def cmd_doctor(args):
    platform_root = find_platform_root()
    root = find_workspace_root(platform_root)
    catalog = load_catalog(platform_root)
    print(f"=== Cyrene Workspace Doctor ===")
    print(f"Checking workspace at: {root}\n")

    issues = []

    # 1. Root Git check
    if (root / ".git").exists():
        issues.append("[WARNING] Cyrene workspace root is initialized as a Git repository. Cyrene root should be a container, not a Git repository.")

    # 2. Outer folder naming check
    if root.name.lower() != "cyrene":
        print(f"[INFO] Outer directory is '{root.name}'. Canonical naming is 'Cyrene'.")

    # 3. Toolchain checks
    for tool in ["git", "python", "cargo", "uv", "dotnet"]:
        found = shutil.which(tool) is not None
        status = "[OK]" if found else "[MISSING]"
        print(f"  {tool:<10} : {status}")

    # 4. Repository layout and policy check
    for key, info in catalog.get("repositories", {}).items():
        rel = info.get("canonical_path", key)
        p = platform_root if key == "platform" else root / rel
        if p.exists():
            if not (p / ".git").exists():
                issues.append(f"[ERROR] {rel} exists but is not a Git repository!")
            policy_file = p / "repository-policy.yaml"
            if not policy_file.exists():
                issues.append(f"[WARNING] {rel} is missing repository-policy.yaml")

    print("\n=== Doctor Summary ===")
    if issues:
        for issue in issues:
            print(" ", issue)
    else:
        print(" [OK] All workspace structure and policies are fully healthy!")

def cmd_hydrate(args):
    platform_root = find_platform_root()
    root = find_workspace_root(platform_root)
    catalog = load_catalog(platform_root)
    baseline = load_baseline(platform_root)

    profile = args.profile
    profile_info = catalog.get("profiles", {}).get(profile)
    if not profile_info:
        print(f"[ERROR] Unknown profile '{profile}'. Available: {list(catalog.get('profiles', {}).keys())}")
        sys.exit(1)

    target_repos = profile_info.get("repositories", [])
    closure = resolve_dependency_closure(catalog, target_repos)

    print(f"=== Hydrating Cyrene Profile: {profile} ===")
    print(f"Target Repositories: {target_repos}")
    print(f"Resolved Dependency Closure: {sorted(list(closure))}\n")

    cloned_count = 0
    skipped_count = 0

    for key in sorted(list(closure)):
        info = catalog.get("repositories", {}).get(key, {})
        name = info.get("logical_name", key)
        rel_path = info.get("canonical_path", key)
        remote = info.get("remote")
        target_path = platform_root if key == "platform" else root / rel_path

        if target_path.exists():
            print(f"  [EXISTS] {name:<20} at {rel_path} (Untouched)")
            skipped_count += 1
            continue

        print(f"  [CLONING] {name:<20} from {remote} -> {rel_path}...")
        target_path.parent.mkdir(parents=True, exist_ok=True)
        res = subprocess.run(["git", "clone", remote, str(target_path)], capture_output=True, text=True)
        if res.returncode != 0:
            print(f"    [FAILED] Failed to clone {name}: {res.stderr.strip()}")
        else:
            cloned_count += 1
            # Checkout baseline ref if provided
            base_ref = baseline.get(key, {}).get("ref")
            if base_ref and args.use_baseline:
                subprocess.run(["git", "-C", str(target_path), "checkout", base_ref], capture_output=True, text=True)
                print(f"    [CHECKOUT] Checked out baseline ref {base_ref[:7]}")

    print(f"\nHydration Complete: {cloned_count} cloned, {skipped_count} existing.")

def main():
    parser = argparse.ArgumentParser(description="Cyrene Workspace Orchestrator")
    sub = parser.add_subparsers(dest="command", required=True)

    p_stat = sub.add_parser("status", help="Show workspace repository status table")
    p_stat.set_defaults(func=cmd_status)

    p_doc = sub.add_parser("doctor", help="Check workspace health and toolchains")
    p_doc.set_defaults(func=cmd_doctor)

    p_hyd = sub.add_parser("bootstrap", aliases=["hydrate"], help="Hydrate repositories for a specific profile")
    p_hyd.add_argument("--profile", default="training", choices=["core", "training", "serving", "gateway", "full"], help="Target profile")
    p_hyd.add_argument("--use-baseline", action="store_true", help="Checkout exact baseline refs for newly cloned dependencies")
    p_hyd.set_defaults(func=cmd_hydrate)

    args = parser.parse_args()
    args.func(args)

if __name__ == "__main__":
    main()
