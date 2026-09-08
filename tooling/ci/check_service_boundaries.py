#!/usr/bin/env python3
"""Validate source boundaries owned by the Cyrene-Platform repository."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import NamedTuple


class Violation(NamedTuple):
    rule: str
    file_path: str
    line_number: int
    content: str
    message: str


IGNORED_PARTS = {
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

CODE_EXTENSIONS = {".py", ".cs", ".kt", ".rs", ".go", ".ts", ".js"}


def find_platform_root(start_path: Path | None = None) -> Path:
    """Locate the current Platform checkout without discovering siblings."""
    current = (start_path or Path(__file__)).resolve()
    for parent in [current, *current.parents]:
        if (parent / "repository-policy.yaml").is_file() and (parent / "kernel" / "crates").is_dir():
            return parent
    raise RuntimeError("Could not locate the Cyrene-Platform repository root")


def tracked_source_files(root: Path):
    """Yield Platform source files while excluding generated and build trees."""
    for path in root.rglob("*"):
        if not path.is_file() or path.suffix not in CODE_EXTENSIONS:
            continue
        if any(part in IGNORED_PARTS for part in path.relative_to(root).parts):
            continue
        yield path


def check_service_boundaries(platform_root: Path) -> list[Violation]:
    """Check Kernel purity and prevent imports of private implementations."""
    violations: list[Violation] = []
    kernel_dir = platform_root / "kernel" / "crates"

    for path in tracked_source_files(kernel_dir):
        if path.suffix != ".rs":
            continue
        text = path.read_text(encoding="utf-8", errors="ignore")
        for line_number, line in enumerate(text.splitlines(), 1):
            if re.search(r"\b(cy_exec|TrainingSpec|ModelManifest|cy_platform_api)\b", line):
                violations.append(
                    Violation(
                        rule="KERNEL_PURITY_VIOLATION",
                        file_path=str(path.relative_to(platform_root)),
                        line_number=line_number,
                        content=line.strip(),
                        message="Kernel references a high-level Product or framework type",
                    )
                )

    private_import = re.compile(
        r"^\s*(import|from|using|extern crate)\s+.*"
        r"\b(cyrene_enterprise|cyrene_commercial|CyreneEnterprise|CyreneCommercial)\b"
    )
    for path in tracked_source_files(platform_root):
        text = path.read_text(encoding="utf-8", errors="ignore")
        for line_number, line in enumerate(text.splitlines(), 1):
            if private_import.search(line):
                violations.append(
                    Violation(
                        rule="NO_PRIVATE_PACKAGE_IMPORT",
                        file_path=str(path.relative_to(platform_root)),
                        line_number=line_number,
                        content=line.strip(),
                        message="Platform source imports a private implementation package",
                    )
                )

    return violations


def main() -> int:
    platform_root = find_platform_root()
    print(f"Checking Platform source boundaries in: {platform_root}")
    violations = check_service_boundaries(platform_root)

    if violations:
        print(f"\n[ERROR] Found {len(violations)} boundary violation(s):")
        for violation in violations:
            print(
                f"  [{violation.rule}] {violation.file_path}:{violation.line_number}\n"
                f"    Code: {violation.content}\n"
                f"    Issue: {violation.message}\n"
            )
        return 1

    print("\n[OK] Platform source and Kernel boundaries passed cleanly!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
