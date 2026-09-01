#!/usr/bin/env python3
"""Service Boundary & Repository Topology Governance Guard.

Enforces strict architectural boundaries across Cyrene:
1. Services must not import or reference top-level plugins/ via relative paths.
2. Services must not depend on infrastructure/ as runtime code.
3. Services must not depend on tooling/ as production runtime code.
4. Plugins must not depend on concrete service implementations in services/.
5. Kernel Core (Cyrene-Platform/kernel/crates/) must remain pure and free of
   Product, Service, or Plugin semantics.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import List, NamedTuple


class Violation(NamedTuple):
    rule: str
    file_path: str
    line_number: int
    content: str
    message: str


def find_workspace_root(start_path: Path | None = None) -> Path:
    """Locate the umbrella workspace or fall back to this Platform checkout.

    The guard also runs in standalone GitHub checkouts.  A machine-local
    Windows fallback made those runs inspect a path that cannot exist on the
    runner, so standalone validation must remain rooted at the checked-out
    repository instead.
    """
    current = (start_path or Path(__file__)).resolve()
    for parent in [current] + list(current.parents):
        if (parent / "Cyrene-Platform").exists() and (parent / "services").exists():
            return parent
        if (parent / "tooling" / "ci" / "check_service_boundaries.py").exists() and (
            (parent / "kernel").is_dir() or (parent / "Cargo.toml").is_file()
        ):
            return parent
    return current.parent.parent


def check_service_boundaries(workspace_root: Path) -> List[Violation]:
    violations: List[Violation] = []
    services_dir = workspace_root / "services"
    plugins_dir = workspace_root / "plugins"
    kernel_dir = workspace_root / "Cyrene-Platform" / "kernel" / "crates"

    ignored_dirs = {
        ".git",
        ".venv",
        "node_modules",
        "obj",
        "bin",
        "__pycache__",
        ".pytest_cache",
        "target",
        ".idea",
        ".vscode",
    }

    code_extensions = {
        ".py",
        ".cs",
        ".kt",
        ".rs",
        ".go",
        ".ts",
        ".js",
        ".dart",
        ".toml",
        ".props",
        ".csproj",
    }

    # 1. Check Services
    if services_dir.exists():
        for sdir in services_dir.iterdir():
            if not sdir.is_dir() or sdir.name.endswith("-worktree"):
                continue
            for root, dirs, files in os.walk(sdir):
                dirs[:] = [d for d in dirs if d not in ignored_dirs]
                for fname in files:
                    fpath = Path(root) / fname
                    if fpath.suffix not in code_extensions:
                        continue
                    try:
                        text = fpath.read_text(encoding="utf-8", errors="ignore")
                    except Exception:
                        continue

                    rel_path = fpath.relative_to(workspace_root)
                    for idx, line in enumerate(text.splitlines(), 1):
                        # Rule 1: No relative imports to plugins/
                        if re.search(r'[\"\']\.\./+(\.\./+)*plugins/', line):
                            violations.append(
                                Violation(
                                    rule="NO_RELATIVE_PLUGIN_IMPORT",
                                    file_path=str(rel_path),
                                    line_number=idx,
                                    content=line.strip(),
                                    message="Service imports concrete plugin implementation via relative path",
                                )
                            )
                        # Rule 2: No relative imports to infrastructure/
                        if re.search(r'[\"\']\.\./+(\.\./+)*infrastructure/', line):
                            violations.append(
                                Violation(
                                    rule="NO_INFRASTRUCTURE_RUNTIME_IMPORT",
                                    file_path=str(rel_path),
                                    line_number=idx,
                                    content=line.strip(),
                                    message="Service imports infrastructure configuration as runtime code",
                                )
                            )
                        # Rule 3: No relative imports to tooling/
                        if re.search(r'[\"\']\.\./+(\.\./+)*tooling/', line):
                            violations.append(
                                Violation(
                                    rule="NO_TOOLING_RUNTIME_IMPORT",
                                    file_path=str(rel_path),
                                    line_number=idx,
                                    content=line.strip(),
                                    message="Service imports tooling code as production runtime dependency",
                                )
                            )

    # 2. Check Plugins -> Services isolation
    if plugins_dir.exists():
        for pdir in plugins_dir.iterdir():
            if not pdir.is_dir():
                continue
            for root, dirs, files in os.walk(pdir):
                dirs[:] = [d for d in dirs if d not in ignored_dirs]
                for fname in files:
                    fpath = Path(root) / fname
                    if fpath.suffix not in code_extensions:
                        continue
                    try:
                        text = fpath.read_text(encoding="utf-8", errors="ignore")
                    except Exception:
                        continue

                    rel_path = fpath.relative_to(workspace_root)
                    for idx, line in enumerate(text.splitlines(), 1):
                        if re.search(r'[\"\']\.\./+(\.\./+)*services/', line):
                            violations.append(
                                Violation(
                                    rule="NO_PLUGIN_TO_SERVICE_IMPORT",
                                    file_path=str(rel_path),
                                    line_number=idx,
                                    content=line.strip(),
                                    message="Plugin imports concrete service implementation",
                                )
                            )

    # 3. Check Kernel Core purity
    if kernel_dir.exists():
        for root, dirs, files in os.walk(kernel_dir):
            dirs[:] = [d for d in dirs if d not in ignored_dirs]
            for fname in files:
                fpath = Path(root) / fname
                if fpath.suffix != ".rs":
                    continue
                try:
                    text = fpath.read_text(encoding="utf-8", errors="ignore")
                except Exception:
                    continue

                rel_path = fpath.relative_to(workspace_root)
                for idx, line in enumerate(text.splitlines(), 1):
                    if re.search(r'\b(cy_exec|TrainingSpec|ModelManifest|cy_platform_api)\b', line):
                        violations.append(
                            Violation(
                                rule="KERNEL_PURITY_VIOLATION",
                                file_path=str(rel_path),
                                line_number=idx,
                                content=line.strip(),
                                message="Kernel Core references high-level Product or Platform Framework type",
                            )
                        )

    # 4. Check Public Foundation does not import Private Enterprise/Commercial packages
    public_dirs = [workspace_root / "Cyrene-Platform", workspace_root / "plugins"]
    for pdir in public_dirs:
        if not pdir.exists():
            continue
        for root, dirs, files in os.walk(pdir):
            dirs[:] = [d for d in dirs if d not in ignored_dirs and d != "tooling"]
            for fname in files:
                fpath = Path(root) / fname
                if fpath.suffix not in [".py", ".rs", ".cs", ".kt"]:
                    continue
                try:
                    text = fpath.read_text(encoding="utf-8", errors="ignore")
                except Exception:
                    continue
                rel_path = fpath.relative_to(workspace_root)
                for idx, line in enumerate(text.splitlines(), 1):
                    if re.search(r'^\s*(import|from|using|extern crate)\s+.*\b(cyrene_enterprise|cyrene_commercial|CyreneEnterprise|CyreneCommercial)\b', line):
                        violations.append(
                            Violation(
                                rule="NO_PRIVATE_PACKAGE_IMPORT",
                                file_path=str(rel_path),
                                line_number=idx,
                                content=line.strip(),
                                message="Public foundational code imports or references private package",
                            )
                        )

    # Rule 5: CI Authority and Public Workflow Alignment
    repo_dirs = [workspace_root / "Cyrene-Platform", workspace_root / "plugins"]
    services_dir = workspace_root / "services"
    if services_dir.exists():
        for s in services_dir.iterdir():
            if s.is_dir() and (s / ".git").exists() and not s.name.endswith("-worktree") and not (s / ".git").is_file():
                repo_dirs.append(s)

    for rdir in repo_dirs:
        policy_file = rdir / "repository-policy.yaml"
        if not policy_file.exists():
            continue
        try:
            p_text = policy_file.read_text(encoding="utf-8")
            if 'visibility: "public"' in p_text or "visibility: public" in p_text:
                if 'ci: "github"' in p_text or "ci: github" in p_text:
                    gh_wf = rdir / ".github" / "workflows"
                    if not gh_wf.exists() or not list(gh_wf.glob("*.yml")):
                        violations.append(
                            Violation(
                                rule="MISSING_PUBLIC_CI_WORKFLOW",
                                file_path=str(rdir.relative_to(workspace_root)),
                                line_number=1,
                                content=str(gh_wf),
                                message="Public repository policy declares ci_authority=github but missing active GitHub workflow",
                            )
                        )
        except Exception:
            pass

    return violations



def main() -> int:
    workspace_root = find_workspace_root()
    print(f"Checking Service and Repository Boundaries in: {workspace_root}")
    violations = check_service_boundaries(workspace_root)

    if violations:
        print(f"\n[ERROR] Found {len(violations)} boundary violation(s):")
        for v in violations:
            print(f"  [{v.rule}] {v.file_path}:{v.line_number}")
            print(f"    Code: {v.content}")
            print(f"    Issue: {v.message}\n")
        return 1

    print("\n[OK] All Service, Plugin, Infrastructure, and Kernel boundaries passed clean!")
    return 0



if __name__ == "__main__":
    sys.exit(main())
