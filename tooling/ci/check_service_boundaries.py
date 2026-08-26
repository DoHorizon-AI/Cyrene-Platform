#!/usr/bin/env python3
"""Service Boundary & Repository Governance Guard.

Authoritative CI check for workspace layer isolation:
1. Services must not import or reference top-level plugins/ via relative paths.
2. Services must not depend on infrastructure/ as runtime code.
3. Services must not depend on tooling/ as production runtime code.
4. Plugins must not depend on concrete service implementations in services/.

Note: Kernel Core purity is authoritatively verified by check-kernel-boundary.sh
and check-kernel-semantic-contract.sh; this script focuses strictly on
Service <-> Plugin <-> Infrastructure <-> Tooling boundaries.
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
    current = (start_path or Path(__file__)).resolve()
    for parent in [current] + list(current.parents):
        if (parent / "Cyrene-Platform").exists() and (parent / "services").exists():
            return parent
    raise RuntimeError(f"Could not locate Cyrene workspace root from {current}")


def check_service_boundaries(workspace_root: Path) -> List[Violation]:
    violations: List[Violation] = []
    services_dir = workspace_root / "services"
    plugins_dir = workspace_root / "plugins"

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

    # 1. Check Services -> Plugins / Infrastructure / Tooling isolation
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

    return violations


def main() -> int:
    workspace_root = find_workspace_root()
    print(f"Checking Service and Repository Boundaries in: {workspace_root}")
    violations = check_service_boundaries(workspace_root)

    if violations:
        print(f"\n❌ Found {len(violations)} boundary violation(s):")
        for v in violations:
            print(f"  [{v.rule}] {v.file_path}:{v.line_number}")
            print(f"    Code: {v.content}")
            print(f"    Issue: {v.message}\n")
        return 1

    print("\n✅ All Service, Plugin, Infrastructure, and Tooling boundaries passed clean!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
