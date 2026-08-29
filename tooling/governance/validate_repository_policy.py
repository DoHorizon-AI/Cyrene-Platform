#!/usr/bin/env python3
"""
Cyrene Repository Policy Validator
Validates machine-readable repository-policy.yaml files and local lifecycle docs across all repositories.
"""

import os
import sys
import yaml
from pathlib import Path

def find_workspace_root() -> Path:
    curr = Path.cwd().resolve()
    parents = [curr] + list(curr.parents)
    for parent in parents:
        if (parent / "Cyrene-Platform").exists() and (parent / "plugins").exists():
            return parent
    for parent in parents:
        if (parent / "repository-policy.yaml").is_file() and (
            parent / "docs" / "REPOSITORY-LIFECYCLE.md"
        ).is_file():
            return parent
    return curr

VALID_CLASSES = {
    "PUBLIC_FOUNDATION",
    "PUBLIC_PRODUCT",
    "PUBLIC_COMPONENT_COLLECTION",
    "PRIVATE_COMMERCIAL",
    "PRIVATE_INTERNAL",
}

VALID_ROLES = {
    "USER_DISTRIBUTION",
    "COMPONENT_RELEASE",
    "SOURCE_ONLY",
    "INTERNAL_DEPLOYMENT_ONLY",
    "MULTI_COMPONENT_COLLECTION",
}

def validate_all_repository_policies(workspace_root: Path) -> list:
    errors = []

    # Discover repos
    platform_dir = workspace_root / "Cyrene-Platform"
    if not platform_dir.is_dir():
        platform_dir = workspace_root
    repo_dirs = [platform_dir]
    plugins_dir = workspace_root / "plugins"
    if plugins_dir.is_dir():
        repo_dirs.append(plugins_dir)
    services_dir = workspace_root / "services"
    if services_dir.exists():
        for s in services_dir.iterdir():
            if s.is_dir() and (s / ".git").exists():
                # Skip secondary linked git worktrees (e.g. -worktree)
                if s.name.endswith("-worktree") or (s / ".git").is_file():
                    continue
                repo_dirs.append(s)


    print(f"Discovered {len(repo_dirs)} repositories in workspace: {workspace_root}\n")

    for r in repo_dirs:
        rel_name = str(r.relative_to(workspace_root))
        policy_file = r / "repository-policy.yaml"
        lifecycle_doc = r / "docs" / "REPOSITORY-LIFECYCLE.md"

        if not policy_file.exists():
            errors.append(f"Missing repository-policy.yaml in {rel_name}")
            continue

        try:
            policy = yaml.safe_load(policy_file.read_text(encoding="utf-8"))
        except Exception as e:
            errors.append(f"Failed to parse YAML in {rel_name}/repository-policy.yaml: {e}")
            continue

        # Check required fields
        repo_info = policy.get("repository", {})
        lclass = repo_info.get("lifecycle_class")
        vis = repo_info.get("visibility")
        if lclass not in VALID_CLASSES:
            errors.append(f"{rel_name}: Invalid lifecycle_class '{lclass}'")

        dist = policy.get("distribution", {})
        role = dist.get("role")
        if role not in VALID_ROLES:
            errors.append(f"{rel_name}: Invalid distribution.role '{role}'")

        auth = policy.get("authorities", {})
        ci_auth = auth.get("ci")
        if vis == "public" and ci_auth != "github":
            errors.append(f"{rel_name}: Public repository must have ci_authority 'github', found '{ci_auth}'")

        trust = policy.get("trust", {})
        if vis == "public" and trust.get("public_requires_private", False) is True:
            errors.append(f"{rel_name}: Public repository must not declare public_requires_private=True")

        # Check lifecycle doc exists
        if not lifecycle_doc.exists():
            errors.append(f"Missing docs/REPOSITORY-LIFECYCLE.md in {rel_name}")
        else:
            doc_text = lifecycle_doc.read_text(encoding="utf-8")
            if lclass not in doc_text:
                errors.append(f"{rel_name}: docs/REPOSITORY-LIFECYCLE.md does not match lifecycle_class '{lclass}'")
            if role not in doc_text:
                errors.append(f"{rel_name}: docs/REPOSITORY-LIFECYCLE.md does not match distribution.role '{role}'")

    return errors

def main():
    root = find_workspace_root()
    print(f"Validating Repository Policies in: {root}\n")
    errors = validate_all_repository_policies(root)

    if errors:
        print(f"[ERROR] Found {len(errors)} repository policy error(s):")
        for e in errors:
            print("  ", e)
        sys.exit(1)

    print("[SUCCESS] All repository-policy.yaml files and local lifecycle docs validated 100% cleanly!")
    sys.exit(0)

if __name__ == "__main__":
    main()
