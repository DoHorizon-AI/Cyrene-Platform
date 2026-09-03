#!/usr/bin/env python3
"""
Cyrene Multi-Repository Integration Test Orchestrator
Supports deterministic planning, clean CI workspace hydration, and execution of cross-repo test suites.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import yaml
from pathlib import Path

def find_workspace_root() -> Path:
    curr = Path.cwd().resolve()
    for parent in [curr] + list(curr.parents):
        if (parent / "Cyrene-Platform").exists():
            return parent
        if (parent / "tooling" / "workspace" / "repos.yaml").is_file():
            # Public CI checks out Platform alone; treat that checkout as the
            # local root while preserving the umbrella layout when present.
            # 公共 CI 只 checkout Platform 时，将该 checkout 视为本地根目录；
            # 若存在 umbrella layout，则仍按多仓 workspace 处理。
            return parent
    return curr


def platform_root(workspace_root: Path) -> Path:
    """Resolve Platform in either umbrella or standalone checkout layout."""
    candidate = workspace_root / "Cyrene-Platform"
    return candidate if (candidate / "tooling" / "workspace" / "repos.yaml").is_file() else workspace_root


def load_catalog(workspace_root: Path) -> dict:
    cat_file = platform_root(workspace_root) / "tooling" / "workspace" / "repos.yaml"
    if not cat_file.exists():
        return {}
    return yaml.safe_load(cat_file.read_text(encoding="utf-8")) or {}

def load_baseline(workspace_root: Path) -> dict:
    base_file = platform_root(workspace_root) / "tooling" / "workspace" / "workspace-baseline.yaml"
    if not base_file.exists():
        return {}
    return yaml.safe_load(base_file.read_text(encoding="utf-8")) or {}

def resolve_dependency_closure(catalog: dict, target_repos: list) -> set:
    closure = set()
    stack = list(target_repos)
    while stack:
        r = stack.pop()
        if r not in closure:
            closure.add(r)
            for d in catalog.get("repositories", {}).get(r, {}).get("dependencies", []):
                if d not in closure:
                    stack.append(d)
    return closure

def plan_integration(workspace_root: Path, profile: str, overrides: dict = None, mode: str = "local") -> dict:
    catalog = load_catalog(workspace_root)
    baseline_data = load_baseline(workspace_root)
    baseline = baseline_data.get("repositories", {})

    profile_repos = catalog.get("profiles", {}).get(profile, {}).get("repositories", [])
    if not profile_repos:
        print(f"[ERROR] Unknown profile '{profile}'")
        sys.exit(1)

    closure = resolve_dependency_closure(catalog, profile_repos)
    plan = {}

    for r in sorted(list(closure)):
        info = catalog.get("repositories", {}).get(r, {})
        rel_path = info.get("canonical_path", r)
        p = (
            workspace_root
            if r == "platform" and platform_root(workspace_root) == workspace_root
            else workspace_root / rel_path
        )

        # Determine ref based on mode
        ref = (overrides or {}).get(r)
        ref_source = "override" if ref else "auto"

        if not ref:
            if mode == "local" and p.exists() and (p / ".git").exists():
                ref = subprocess.run(["git", "-C", str(p), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
                ref_source = "local_head"
            else:
                # Use remote baseline ref
                b_entry = baseline.get(r, {})
                ref = b_entry.get("remote_ref") or b_entry.get("ref", "UNRESOLVED")
                ref_source = "remote_baseline"

        # Determine test commands
        test_cmds = []
        if r == "platform":
            test_cmds.append([sys.executable, "-m", "pytest", "sdk/python/cyrene_control_plane/tests", "sdk/python/cyrene_preflight/tests"])
        elif r == "yield":
            test_cmds.append([sys.executable, "-m", "pytest", "training/core/tests"])
        elif r == "plugins":
            test_cmds.append([sys.executable, "-m", "pytest", "conformance/tests"])
        elif r == "reactor":
            test_cmds.append([sys.executable, "-m", "pytest", "runtime/core/tests"])

        plan[r] = {
            "logical_name": info.get("logical_name", r),
            "canonical_path": rel_path,
            "remote": info.get("remote"),
            "ref": ref,
            "ref_source": ref_source,
            "present": p.exists(),
            "test_cmds": test_cmds,
        }

    return plan

def execute_integration(workspace_root: Path, plan: dict, ci_workspace: Path = None, current_repo: str = None, current_ref: str = None) -> dict:
    target_root = ci_workspace if ci_workspace else workspace_root
    report = {
        "target_root": str(target_root),
        "is_ci_workspace": bool(ci_workspace),
        "repositories": {},
        "overall_status": "PASSED",
    }

    print(f"\n=== Executing Cyrene Multi-Repository Integration ===")
    print(f"Target Workspace: {target_root}\n")

    # If CI workspace mode, hydrate missing repos
    if ci_workspace:
        ci_workspace.mkdir(parents=True, exist_ok=True)
        for r_key, item in plan.items():
            dest = ci_workspace / item["canonical_path"]
            if r_key == current_repo and dest.exists():
                print(f"  [CURRENT PR] {item['logical_name']} at {item['canonical_path']} (Ref: {current_ref[:7] if current_ref else 'HEAD'})")
            elif not dest.exists():
                print(f"  [HYDRATING] {item['logical_name']} from {item['remote']} (Ref: {item['ref'][:7]})...")
                dest.parent.mkdir(parents=True, exist_ok=True)
                subprocess.run(["git", "clone", item["remote"], str(dest)], capture_output=True, text=True)
                subprocess.run(["git", "-C", str(dest), "checkout", item["ref"]], capture_output=True, text=True)

    # Build unified PYTHONPATH containing Platform SDKs and target repo sources
    plat_dir = platform_root(target_root)
    python_paths = [
        str(plat_dir / "sdk/python/cyrene_control_plane/src"),
        str(plat_dir / "sdk/python/cyrene_artifacts/src"),
        str(plat_dir / "sdk/python/cyrene_environment/src"),
        str(plat_dir / "sdk/python/cyrene_preflight/src"),
        str(target_root / "services/Cyrene-Yield/training/core/src"),
    ]
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join([p for p in python_paths if Path(p).exists()]) + os.pathsep + env.get("PYTHONPATH", "")

    # Run tests
    for r_key, item in plan.items():
        repo_dir = target_root / item["canonical_path"]
        if not repo_dir.exists():
            print(f"  [ERROR] Required dependency {item['logical_name']} is missing at {repo_dir}!")
            report["repositories"][r_key] = {"status": "FAILED", "error": "Missing repository"}
            report["overall_status"] = "FAILED"
            continue

        actual_sha = subprocess.run(["git", "-C", str(repo_dir), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        print(f"--- Running tests for {item['logical_name']} (SHA: {actual_sha[:10]}) ---")

        repo_passed = True
        for cmd in item.get("test_cmds", []):
            print(f"  Executing: {' '.join(cmd)} in {item['canonical_path']}")
            res = subprocess.run(cmd, cwd=str(repo_dir), env=env, capture_output=True, text=True)
            if res.returncode != 0:
                print(f"  [FAILED] {res.stderr.strip() or res.stdout.strip()}")
                repo_passed = False
            else:
                print("  [OK] Test suite passed.")


        report["repositories"][r_key] = {
            "logical_name": item["logical_name"],
            "sha": actual_sha,
            "ref_source": item["ref_source"],
            "status": "PASSED" if repo_passed else "FAILED",
        }
        if not repo_passed:
            report["overall_status"] = "FAILED"

    report_file = target_root / "integration-report.json"
    try:
        report_file.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"\nIntegration report written to: {report_file}")
    except Exception:
        pass

    return report

def main():
    parser = argparse.ArgumentParser(description="Cyrene Multi-Repository Integration Orchestrator")
    parser.add_argument("--profile", default="training", choices=["core", "training", "serving", "gateway", "full"], help="Target profile")
    parser.add_argument("--plan", "--dry-run", action="store_true", help="Print deterministic integration plan without running tests")

    parser.add_argument("--execute", action="store_true", help="Execute integration tests against workspace")
    parser.add_argument("--mode", choices=["local", "remote_ci"], default="local", help="Baseline resolution mode")
    parser.add_argument("--override", action="append", help="Override dependency ref in format repo=ref")
    parser.add_argument("--ci-workspace", help="Path to temporary clean CI workspace directory")
    parser.add_argument("--current-repo", help="Identifier of current repository being tested")
    parser.add_argument("--current-ref", help="Exact SHA/ref of current repository")

    args = parser.parse_args()
    root = find_workspace_root()

    overrides = {}
    if args.override:
        for ov in args.override:
            if "=" in ov:
                k, v = ov.split("=", 1)
                overrides[k.strip()] = v.strip()

    plan = plan_integration(root, args.profile, overrides, mode=args.mode)

    if args.plan or not args.execute:
        print(f"=== Cyrene Integration Plan (Profile: {args.profile}, Mode: {args.mode}) ===")
        for k, v in plan.items():
            print(f"  {v['logical_name']:<20} | Path: {v['canonical_path']:<25} | Ref: {v['ref'][:10]} ({v['ref_source']}) | Present: {v['present']}")
        if not args.execute:
            sys.exit(0)

    ci_ws = Path(args.ci_workspace).resolve() if args.ci_workspace else None
    report = execute_integration(root, plan, ci_workspace=ci_ws, current_repo=args.current_repo, current_ref=args.current_ref)

    if report["overall_status"] == "PASSED":
        print("\n[SUCCESS] Multi-repository integration passed 100% cleanly!")
        sys.exit(0)
    else:
        print("\n[FAILURE] Multi-repository integration failed.")
        sys.exit(1)

if __name__ == "__main__":
    main()
