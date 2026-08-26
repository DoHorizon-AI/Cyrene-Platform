#!/usr/bin/env python3
"""
Cyrene Workspace CLI Tooling
Provides status inspection, environment doctor diagnostics, and safe profile bootstrapping.
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

# Locate workspace root (parent of Cyrene-Platform if run from inside, or directory containing Cyrene-Platform)
def find_workspace_root() -> Path:
    curr = Path.cwd().resolve()
    # Check if curr is workspace root containing Cyrene-Platform
    if (curr / "Cyrene-Platform").exists() or (curr / "plugins").exists():
        return curr
    # Check parents
    for parent in [curr] + list(curr.parents):
        if (parent / "Cyrene-Platform").exists() and (parent / "plugins").exists():
            return parent
        if parent.name == "Cyrene-Platform" and parent.parent.exists():
            return parent.parent
    return curr

WORKSPACE_ROOT = find_workspace_root()

def load_catalog() -> dict:
    catalog_path = Path(__file__).resolve().parent / "repos.yaml"
    # Basic YAML-like parser using simple line reading to avoid external pyyaml dependency
    repos = {}
    if catalog_path.exists():
        current_key = None
        current_repo = {}
        for line in catalog_path.read_text(encoding="utf-8").splitlines():
            sline = line.strip()
            if not sline or sline.startswith("#"):
                continue
            if line.startswith("  ") and not line.startswith("    ") and sline.endswith(":"):
                if current_key and current_repo:
                    repos[current_key] = current_repo
                current_key = sline[:-1].strip()
                current_repo = {}
            elif line.startswith("    ") and ":" in sline:
                k, v = sline.split(":", 1)
                k = k.strip()
                v = v.strip().strip('"').strip("'")
                if v.startswith("[") and v.endswith("]"):
                    v = [item.strip().strip('"').strip("'") for item in v[1:-1].split(",")]
                elif v.lower() == "true":
                    v = True
                elif v.lower() == "false":
                    v = False
                current_repo[k] = v
        if current_key and current_repo:
            repos[current_key] = current_repo
    return repos

def cmd_status(args):
    print(f"=== Cyrene Workspace Status (Root: {WORKSPACE_ROOT}) ===\n")
    catalog = load_catalog()
    
    header = f"{'Repository':<22} {'Branch':<28} {'Status':<12} {'Worktrees':<10} {'Remote Target'}"
    print(header)
    print("-" * len(header) + "-" * 20)

    for key, info in catalog.items():
        rel_path = info.get("path", key)
        p = WORKSPACE_ROOT / rel_path
        if not p.exists():
            print(f"{info.get('name', key):<22} {'[MISSING]':<28} {'N/A':<12} {'0':<10} {info.get('remote', 'None')}")
            continue

        git_dir = p / ".git"
        if not git_dir.exists():
            print(f"{info.get('name', key):<22} {'[NOT A GIT REPO]':<28} {'N/A':<12} {'0':<10} {info.get('remote', 'None')}")
            continue

        branch = subprocess.run(["git", "-C", str(p), "branch", "--show-current"], capture_output=True, text=True).stdout.strip()
        if not branch:
            branch = "(detached HEAD)"
        status_raw = subprocess.run(["git", "-C", str(p), "status", "--short"], capture_output=True, text=True).stdout.strip()
        status = "Clean" if not status_raw else "Dirty"
        
        wt_out = subprocess.run(["git", "-C", str(p), "worktree", "list", "--porcelain"], capture_output=True, text=True).stdout.strip()
        wt_count = len([w for w in wt_out.split("worktree ") if w.strip()])

        remotes = subprocess.run(["git", "-C", str(p), "remote", "get-url", "origin"], capture_output=True, text=True).stdout.strip()
        if not remotes:
            remotes = "No remote configured"

        print(f"{info.get('name', key):<22} {branch:<28} {status:<12} {wt_count:<10} {remotes}")
    print()

def cmd_doctor(args):
    print("=== Cyrene Developer Environment Doctor ===\n")
    checks = [
        ("Git", ["git", "--version"]),
        ("Python", [sys.executable, "--version"]),
        ("Rust (cargo)", ["cargo", "--version"]),
        (".NET SDK", ["dotnet", "--version"]),
        ("Java (JDK)", ["java", "-version"]),
        ("uv (fast package manager)", ["uv", "--version"]),
    ]

    all_ok = True
    for name, cmd in checks:
        exe = cmd[0]
        loc = shutil.which(exe)
        if loc:
            res = subprocess.run(cmd, capture_output=True, text=True)
            ver = (res.stdout or res.stderr).splitlines()[0].strip()
            print(f"  [OK] {name:<26} -> {ver} ({loc})")
        else:
            if name in ["Git", "Python", "Rust (cargo)"]:
                print(f"  [MISSING - REQUIRED] {name:<14} -> Not found in PATH!")
                all_ok = False
            else:
                print(f"  [OPTIONAL - NOT FOUND] {name:<12} -> Optional toolchain not found")

    print("\nChecking Workspace Root Structure:")
    for d in ["Cyrene-Platform", "plugins", "services"]:
        dp = WORKSPACE_ROOT / d
        if dp.exists():
            print(f"  [OK] Found top-level directory: {d}/")
        else:
            print(f"  [WARN] Missing top-level directory: {d}/")

    print("\nDoctor check finished.\n")

def cmd_bootstrap(args):
    profile = args.profile or "core"
    print(f"=== Bootstrapping Workspace for Profile: [{profile}] ===\n")
    catalog = load_catalog()
    
    for key, info in catalog.items():
        profiles = info.get("profiles", ["full"])
        if profile != "full" and profile not in profiles:
            continue

        rel_path = info.get("path", key)
        p = WORKSPACE_ROOT / rel_path
        remote = info.get("remote")

        if p.exists():
            print(f"  [EXISTS] {info.get('name', key)} is already present at {rel_path}.")
            continue

        if not remote or remote == "None":
            print(f"  [SKIPPED] {info.get('name', key)} has no verified public remote configured.")
            continue

        print(f"  [CLONING] Cloning {info.get('name', key)} from {remote} into {rel_path}...")
        p.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", remote, str(p)], check=True)

    print(f"\nBootstrap completed for profile [{profile}].\n")

def main():
    parser = argparse.ArgumentParser(description="Cyrene Workspace Developer Tool")
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("status", help="Show git status across workspace repositories")
    subparsers.add_parser("doctor", help="Inspect local toolchains, runtimes, and dependencies")
    
    boot = subparsers.add_parser("bootstrap", help="Safely clone missing repositories for a profile")
    boot.add_argument("--profile", choices=["core", "training", "serving", "gateway", "full"], default="core")

    args = parser.parse_args()
    if args.command == "status":
        cmd_status(args)
    elif args.command == "doctor":
        cmd_doctor(args)
    elif args.command == "bootstrap":
        cmd_bootstrap(args)

if __name__ == "__main__":
    main()