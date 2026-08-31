#!/usr/bin/env python3
"""Cyrene Repository-Wide API Documentation & Capability Stability Guard.

Modes:
- --mode workspace: Comprehensive multi-repo workspace integration validation across all 10 repositories and workspace tooling.
- --mode standalone: Single-repository CI guard validating only the local repository without requiring sibling checkouts.

Checks:
1. docs/API.md existence and structural status compliance.
2. README.md links to docs/API.md.
3. Zero machine-local absolute paths (file:///, C:\\Users\\, /Users/<name>/, DHDev/Cyrene) in markdown files.
4. Zero escaping relative markdown links (relative links resolving outside repository root must use canonical GitHub URLs).
5. CAPABILITY_INDEX.md stability truth and candidate isolation.
6. All active plugins possess valid manifests and README documentation.
7. Clean separation between ACTIVE_REPOSITORY and ACTIVE_WORKSPACE_TOOLING.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path
from typing import List, Tuple

# Exact classification of all active components in the Cyrene workspace
ACTIVE_REPOSITORIES: List[Tuple[str, str]] = [
    ("Cyrene-Platform", "ACTIVE_PLATFORM"),
    ("plugins", "ACTIVE_PLUGIN_COLLECTION"),
    ("services/cyrene-catalyst", "ACTIVE_PRODUCT"),
    ("services/Cyrene-Yield", "ACTIVE_PRODUCT"),
    ("services/cyrene-reactor", "ACTIVE_PRODUCT"),
    ("services/cyrene-exchange", "ACTIVE_PRODUCT"),
    ("services/cyrene-navigator", "ACTIVE_PRODUCT"),
    ("services/cyrene-echo", "ACTIVE_PRODUCT"),
    ("services/cyrene-astrbot-rev", "REFERENCE_CONSUMER"),
    ("services/cyrene-dh-system-internal", "INTERNAL_ACTIVE"),
]

ACTIVE_WORKSPACE_TOOLING: List[Tuple[str, str]] = [
    ("governance", "ACTIVE_WORKSPACE_TOOLING"),
]

REQUIRED_STATUS_TERMS = [
    "IMPLEMENTED_STABLE",
    "IMPLEMENTED_EXPERIMENTAL",
    "COMPATIBILITY_SHIM",
    "CONTRACT_CANDIDATE",
    "PLANNED",
]

CANDIDATE_CAPABILITIES = [
    "model.registry.v1",
    "model.source.v1",
    "data.processor.v1",
    "evaluation.runner.v1",
]

FORBIDDEN_PATH_PATTERNS = [
    (re.compile(r"file:///+[a-zA-Z]:", re.IGNORECASE), "Machine-local file URI ('file:///C:')"),
    (re.compile(r"[a-zA-Z]:\\[Uu]sers\\[a-zA-Z0-9_-]+\\", re.IGNORECASE), "Machine-local Windows User path ('C:\\Users\\<user>')"),
    (re.compile(r"/[Uu]sers/[a-zA-Z0-9_-]+/(?!test|example)", re.IGNORECASE), "Machine-local Unix User path ('/Users/<user>/')"),
    (re.compile(r"DHDev[/\\]Cyrene", re.IGNORECASE), "Machine-local workspace directory ('DHDev/Cyrene')"),
]

MARKDOWN_LINK_PATTERN = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")


def find_workspace_root() -> Path:
    """Locate the workspace root directory containing Cyrene-Platform."""
    current = Path.cwd().resolve()
    parents = [current] + list(current.parents)
    for parent in parents:
        if (parent / "Cyrene-Platform").is_dir() and (parent / "services").is_dir():
            return parent
    for parent in parents:
        if (parent / "Cargo.toml").is_file() and (parent / "contracts").is_dir():
            return parent
    return current


def platform_root(workspace_root: Path) -> Path:
    nested = workspace_root / "Cyrene-Platform"
    return nested if nested.is_dir() else workspace_root


def check_markdown_links_and_paths(file_path: Path, repo_root: Path) -> List[str]:
    """Scan a markdown file for machine-local paths and cross-repo escaping relative links."""
    errors = []
    rel_path = file_path.relative_to(repo_root).as_posix()
    try:
        content = file_path.read_text(encoding="utf-8", errors="ignore")
    except Exception as e:
        return [f"Failed to read {rel_path}: {e}"]

    for line_num, line in enumerate(content.splitlines(), 1):
        # 1. Prohibit machine-local absolute paths
        for pattern, desc in FORBIDDEN_PATH_PATTERNS:
            if pattern.search(line):
                if "FORBIDDEN_PATH_PATTERNS" in line or "Machine-local" in line:
                    continue
                errors.append(
                    f"Forbidden {desc} in {rel_path}:{line_num} -> '{line.strip()}' (Use repository-relative links or GitHub URLs instead)"
                )

        # 2. Prohibit relative links escaping repository root
        for match in MARKDOWN_LINK_PATTERN.finditer(line):
            text = match.group(1)
            target = match.group(2).strip()

            # Ignore absolute URLs, protocol links, anchors, and doc routing paths
            if target.startswith("http://") or target.startswith("https://") or target.startswith("#") or target.startswith("mailto:") or target.startswith("/"):
                continue

            clean_target = target.split("#")[0].split("?")[0]
            if not clean_target:
                continue

            resolved = (file_path.parent / clean_target).resolve()
            try:
                resolved.relative_to(repo_root)
            except ValueError:
                errors.append(
                    f"Cross-repository escaping relative link in {rel_path}:{line_num} -> [{text}]({target}) resolves outside repository root. Cross-repo references must use canonical GitHub URLs (https://github.com/DoHorizon-AI/...)."
                )

    return errors


def verify_single_repo(repo_dir: Path, rel_path: str, repo_type: str) -> List[str]:
    """Verify documentation rules for a single repository."""
    errors = []
    if not repo_dir.exists():
        return [f"Missing component directory: {rel_path} [{repo_type}]"]

    # 1. Verify docs/API.md
    api_doc = repo_dir / "docs" / "API.md"
    if not api_doc.exists():
        errors.append(f"Component '{rel_path}' [{repo_type}] is missing authoritative docs/API.md")
    else:
        content = api_doc.read_text(encoding="utf-8", errors="ignore")
        if len(content.strip()) < 200:
            errors.append(f"Component '{rel_path}' docs/API.md content is trivially short ({len(content)} chars)")
        has_status = any(term in content for term in REQUIRED_STATUS_TERMS)
        if not has_status:
            errors.append(f"Component '{rel_path}' docs/API.md lacks standard implementation status tags (e.g. IMPLEMENTED_STABLE, CONTRACT_CANDIDATE)")

    # 2. Verify README.md links to docs/API.md
    readme = repo_dir / "README.md"
    if readme.exists():
        readme_text = readme.read_text(encoding="utf-8", errors="ignore")
        if "API.md" not in readme_text and "api.md" not in readme_text:
            errors.append(f"Component '{rel_path}' README.md does not link to docs/API.md")
    else:
        errors.append(f"Component '{rel_path}' is missing README.md")

    # 3. Check for machine-local absolute paths and escaping relative links in all markdown files
    for root, dirs, files in os.walk(repo_dir):
        if ".git" in dirs:
            dirs.remove(".git")
        if "node_modules" in dirs:
            dirs.remove("node_modules")
        if ".venv" in dirs:
            dirs.remove(".venv")
        for f in files:
            if f.endswith(".md"):
                md_path = Path(root) / f
                errors.extend(check_markdown_links_and_paths(md_path, repo_dir))

    return errors


def verify_workspace_api_docs(workspace_root: Path) -> List[str]:
    """Verify all 10 active repositories and workspace tooling."""
    errors = []
    if not (workspace_root / "Cyrene-Platform").is_dir():
        return verify_single_repo(workspace_root, workspace_root.name, "ACTIVE_PLATFORM")
    # Verify 10 standalone git repositories
    for rel_path, repo_type in ACTIVE_REPOSITORIES:
        repo_dir = workspace_root / rel_path
        errors.extend(verify_single_repo(repo_dir, rel_path, repo_type))

    # Verify workspace tooling directory
    for rel_path, tool_type in ACTIVE_WORKSPACE_TOOLING:
        tool_dir = workspace_root / rel_path
        errors.extend(verify_single_repo(tool_dir, rel_path, tool_type))

    return errors


def verify_capability_index(workspace_root: Path) -> List[str]:
    """Verify validity of CAPABILITY_INDEX.md in Platform."""
    errors = []
    index_path = platform_root(workspace_root) / "docs" / "api" / "CAPABILITY_INDEX.md"
    if not index_path.exists():
        return [f"Central Capability Index not found at {index_path}"]

    content = index_path.read_text(encoding="utf-8", errors="ignore")

    # Verify that candidate capabilities are never marked STABLE
    for line in content.splitlines():
        for cand in CANDIDATE_CAPABILITIES:
            if cand in line:
                if "**STABLE**" in line or "| STABLE |" in line:
                    errors.append(f"Candidate capability '{cand}' is illegally marked STABLE in CAPABILITY_INDEX.md")

    return errors


def verify_plugin_docs(workspace_root: Path) -> List[str]:
    """Verify that all active plugins have manifest and readme documentation."""
    errors = []
    plugins_dir = workspace_root / "plugins" / "plugins"
    if not plugins_dir.exists():
        if not (workspace_root / "Cyrene-Platform").is_dir():
            return errors
        return [f"Plugins directory not found at {plugins_dir}"]

    count = 0
    for root, dirs, files in os.walk(plugins_dir):
        if "plugin.manifest.json" in files:
            count += 1
            if "README.md" not in files:
                rel = os.path.relpath(root, workspace_root)
                errors.append(f"Plugin at '{rel}' is missing local README.md documentation")

    if count < 10:
        errors.append(f"Expected at least 10 active plugins, found {count}")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="Cyrene API Documentation & Capability Stability Guard")
    parser.add_argument(
        "--mode",
        choices=["workspace", "standalone"],
        default="workspace",
        help="Verification mode: 'workspace' for full multi-repo integration, 'standalone' for single repository CI",
    )
    args = parser.parse_args()

    workspace_root = find_workspace_root()

    if args.mode == "standalone":
        # Determine local repo directory from CWD
        local_dir = Path.cwd().resolve()
        rel_name = local_dir.name
        print(f"=== Cyrene Standalone Per-Repository API Guard ({rel_name}) ===\n")
        errors = verify_single_repo(local_dir, rel_name, "STANDALONE_REPO")
        if errors:
            print(f"FAIL: Found {len(errors)} documentation discrepancies in {rel_name}:\n")
            for err in errors:
                print(f"  - {err}")
            return 1
        print(f"SUCCESS: Standalone documentation guard passed for {rel_name}!")
        return 0

    # Multi-repo workspace mode
    print(f"=== Cyrene Multi-Repo Workspace API Documentation Guard ===\nWorkspace Root: {workspace_root}\n")

    api_errors = verify_workspace_api_docs(workspace_root)
    cap_errors = verify_capability_index(workspace_root)
    plugin_errors = verify_plugin_docs(workspace_root)

    all_errors = api_errors + cap_errors + plugin_errors

    if all_errors:
        print(f"FAIL: Found {len(all_errors)} documentation/contract discrepancies:\n")
        for err in all_errors:
            print(f"  - {err}")
        return 1

    print("SUCCESS: All 10 active repositories and workspace tooling possess compliant docs/API.md, zero local path leaks, zero cross-repo escaping relative links, and verified capability stability!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
