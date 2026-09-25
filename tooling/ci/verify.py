#!/usr/bin/env python3
"""
Cyrene Unified Local Verification Orchestrator
Provides a single-entry command for developers to run fast, deterministic checks before opening a PR.
中文:Cyrene 统一本地验证编排器。为开发者提供单一入口,在创建 PR 前运行快速且确定性的检查。
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def find_platform_root() -> Path:
    file_dir = Path(__file__).resolve().parent
    for parent in [file_dir] + list(file_dir.parents):
        if (parent / "tooling" / "ci" / "check_service_boundaries.py").exists():
            return parent
    return file_dir.parent.parent


PLATFORM_ROOT = find_platform_root()


def run_step(name: str, cmd: list, cwd: Path = PLATFORM_ROOT) -> bool:
    print(f"--- [RUNNING] {name} ---")
    print(f"Command: {' '.join(cmd)}\n")
    res = subprocess.run(cmd, cwd=str(cwd))
    if res.returncode == 0:
        print(f"\n[PASSED] {name}\n")
        return True
    else:
        print(f"\n[FAILED] {name} (Exit code: {res.returncode})\n")
        return False


def verify_docs() -> bool:
    doc_tool = PLATFORM_ROOT / "tooling" / "docs" / "validate_docs.py"
    return run_step("Documentation Link & Index Validation", [sys.executable, str(doc_tool)])


def verify_governance() -> bool:
    gov_tool = PLATFORM_ROOT / "tooling" / "ci" / "check_service_boundaries.py"
    legacy_guard = PLATFORM_ROOT / "tooling" / "ci" / "check-no-legacy-surface.sh"
    test_gov = PLATFORM_ROOT / "tooling" / "ci" / "test_check_service_boundaries.py"
    policy_tool = PLATFORM_ROOT / "tooling" / "governance" / "validate_repository_policy.py"
    test_policy = PLATFORM_ROOT / "tooling" / "governance" / "tests" / "test_repository_policy.py"
    ok1 = run_step("Service Boundary Governance Guard", [sys.executable, str(gov_tool)])
    ok2 = run_step("Governance Pytest Suite", [sys.executable, "-m", "pytest", str(test_gov)])
    ok3 = run_step("Repository Ownership Guard", ["bash", str(legacy_guard)])
    ok4 = run_step("Repository Policy Validation Guard", [sys.executable, str(policy_tool)])
    ok5 = run_step("Repository Policy Pytest Suite", [sys.executable, "-m", "pytest", str(test_policy)])
    return ok1 and ok2 and ok3 and ok4 and ok5


def verify_python() -> bool:
    tests = [
        PLATFORM_ROOT / "sdk/python/cyrene_preflight/tests",
        PLATFORM_ROOT / "sdk/python/cyrene_artifacts/tests",
        PLATFORM_ROOT / "tooling/docs/tests",
        PLATFORM_ROOT / "tooling/governance/tests",
        PLATFORM_ROOT / "tooling/ci/tests",
    ]
    existing = [str(t) for t in tests if t.exists()]
    tested = run_step(
        "Python SDKs & Tooling Unit Tests",
        [sys.executable, "-m", "pytest"] + existing,
    )
    return tested


def verify_rust() -> bool:
    if not shutil.which("cargo"):
        print("[FAILED] cargo not found in PATH; Rust verification is required")
        return False
    ok1 = run_step("Rust Cargo Format Check", ["cargo", "fmt", "--check"])
    ok2 = run_step("Rust Cargo Check (--locked)", ["cargo", "check", "--locked"])
    ok3 = run_step("Rust Cargo Unit Tests (--locked)", ["cargo", "test", "--locked"])
    return ok1 and ok2 and ok3


def main():
    parser = argparse.ArgumentParser(description="Cyrene Local Verification Orchestrator")
    parser.add_argument(
        "--scope",
        choices=["all-light", "docs", "governance", "python", "rust"],
        default="all-light",
        help="Target scope for verification (default: all-light)",
    )

    args = parser.parse_args()
    print(f"=== Cyrene Verification Orchestrator (Scope: {args.scope}) ===\n")

    results = {}
    if args.scope in ["all-light", "docs"]:
        results["docs"] = verify_docs()
    if args.scope in ["all-light", "governance"]:
        results["governance"] = verify_governance()
    if args.scope in ["all-light", "python"]:
        results["python"] = verify_python()
    if args.scope == "rust":
        results["rust"] = verify_rust()

    print("=== Verification Summary ===")
    all_passed = True
    for k, passed in results.items():
        status = "PASSED" if passed else "FAILED"
        print(f"  {k:<16} : {status}")
        if not passed:
            all_passed = False

    if all_passed:
        print("\n[SUCCESS] All verification steps passed cleanly! Ready for PR.")
        sys.exit(0)
    else:
        print("\n[FAILURE] One or more verification steps failed. Please review output above.")
        sys.exit(1)


if __name__ == "__main__":
    main()
