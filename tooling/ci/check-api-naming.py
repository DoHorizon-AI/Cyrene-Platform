"""
┌─────────────────────────────────────────────────────────────────────┐
│  📄 check-api-naming.py                                               │
│  Module: tooling.ci.check_api_naming                                 │
│  Role: Enforces the Cyrene API Naming Constitution during migration.  │
│                                                                      │
│  模块职责：在迁移期阻止新的 legacy API 命名回流。                     │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Iterable


def git_output(repo_root: Path, *args: str) -> str:
    """Run a read-only Git query and return UTF-8 text."""

    result = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return result.stdout


def source_paths(repo_root: Path, roots: Iterable[str]) -> list[str]:
    """Return tracked and untracked paths below the configured source roots."""

    tracked = git_output(repo_root, "ls-files", "-z", "--", *roots).split("\0")
    untracked = git_output(
        repo_root,
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        *roots,
    ).split("\0")
    return sorted({path for path in tracked + untracked if path})


def is_excluded(path: str, excluded_globs: Iterable[str]) -> bool:
    """Return whether a path is documentation, fixture, or generated output."""

    return any(fnmatch.fnmatch(path, pattern) for pattern in excluded_globs)


def compile_patterns(symbols: Iterable[str]) -> list[tuple[str, re.Pattern[str]]]:
    """Compile exact identifier-boundary patterns for configured symbols."""

    return [
        (symbol, re.compile(rf"(?<![A-Za-z0-9_]){re.escape(symbol)}(?![A-Za-z0-9_])"))
        for symbol in symbols
    ]


def diff_lines(repo_root: Path, base: str | None, paths: list[str]) -> list[tuple[str, int, str]]:
    """Read added lines from a branch diff and local staged/unstaged changes."""

    commands: list[list[str]] = []
    if base:
        commands.append(["git", "diff", "--unified=0", f"{base}...HEAD", "--", *paths])
    else:
        commands.append(["git", "diff", "--unified=0", "HEAD", "--", *paths])
    commands.append(["git", "diff", "--cached", "--unified=0", "--", *paths])

    findings: list[tuple[str, int, str]] = []
    for command in commands:
        output = subprocess.run(
            command,
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        ).stdout
        current_path: str | None = None
        current_line = 0
        for line in output.splitlines():
            if line.startswith("+++ b/"):
                current_path = line[6:]
                continue
            if line.startswith("@@"):
                match = re.search(r"\+(\d+)(?:,(\d+))?", line)
                if match:
                    current_line = int(match.group(1))
                continue
            if current_path is not None and line.startswith("+") and not line.startswith("+++"):
                findings.append((current_path, current_line, line[1:]))
                current_line += 1
            elif current_path is not None and not line.startswith("-"):
                current_line += 1

    tracked = set(git_output(repo_root, "ls-files", "-z", "--", *paths).split("\0"))
    for relative_path in paths:
        if relative_path in tracked:
            continue
        file_path = repo_root / relative_path
        if not file_path.is_file():
            continue
        try:
            content = file_path.read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        findings.extend((relative_path, number, line) for number, line in enumerate(content, 1))
    return findings


def all_source_lines(repo_root: Path, paths: list[str]) -> list[tuple[str, int, str]]:
    """Read all configured source lines for post-migration enforcement."""

    lines: list[tuple[str, int, str]] = []
    for relative_path in paths:
        try:
            content = (repo_root / relative_path).read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        lines.extend((relative_path, number, line) for number, line in enumerate(content, 1))
    return lines


def main() -> int:
    """Validate configured source lines against the forbidden symbol inventory."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mode",
        choices=("changed_source", "all_source"),
        default=None,
        help="Override the policy enforcement mode.",
    )
    parser.add_argument(
        "--config",
        default="tooling/architecture/api-naming.toml",
        help="Path to the machine-readable naming policy.",
    )
    args = parser.parse_args()

    try:
        import tomllib

        repo_root = Path(__file__).resolve().parents[2]
        policy = tomllib.loads((repo_root / args.config).read_text(encoding="utf-8"))
        scope = policy["scope"]
        paths = [
            path
            for path in source_paths(repo_root, scope["roots"])
            if not is_excluded(path, scope["excluded_globs"])
        ]
        mode = args.mode or policy["enforcement"]
        patterns = compile_patterns(policy["forbidden"]["symbols"])
        if mode == "all_source":
            candidates = all_source_lines(repo_root, paths)
        else:
            base_ref = None
            base_name = os.environ.get("GITHUB_BASE_REF")
            if base_name:
                candidate = f"origin/{base_name}"
                verified = subprocess.run(
                    ["git", "rev-parse", "--verify", candidate],
                    cwd=repo_root,
                    capture_output=True,
                    text=True,
                )
                if verified.returncode == 0:
                    base_ref = candidate
            candidates = diff_lines(repo_root, base_ref, paths)
    except (KeyError, OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"API naming gate could not load policy or Git state: {error}", file=sys.stderr)
        return 2

    violations: list[tuple[str, int, str, str]] = []
    for path, line_number, line in candidates:
        for symbol, pattern in patterns:
            if pattern.search(line):
                violations.append((path, line_number, symbol, line.strip()))

    if violations:
        print(f"API naming gate failed in {mode} mode:")
        for path, line_number, symbol, line in violations:
            print(f"  {path}:{line_number}: {symbol}: {line}")
        print("Use the canonical term from docs/governance/API_NAMING_CONSTITUTION.md.")
        return 1

    if mode == "all_source":
        print("API naming gate passed (all_source mode): no forbidden legacy symbol found.")
    else:
        print("API naming gate passed (changed_source mode): no forbidden legacy symbol introduced.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
