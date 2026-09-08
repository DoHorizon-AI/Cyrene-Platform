#!/usr/bin/env python3
"""Validate the policy and lifecycle document of this Platform checkout."""

import sys
from pathlib import Path

import yaml


VALID_CLASSES = {
    "PRIVATE_FOUNDATION",
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


def find_repository_root(start_path: Path | None = None) -> Path:
    """Locate the current repository without discovering sibling checkouts."""
    current = (start_path or Path(__file__)).resolve()
    for parent in [current, *current.parents]:
        if (parent / "repository-policy.yaml").is_file():
            return parent
    raise RuntimeError("Could not locate repository-policy.yaml")


def validate_repository_policy(repository_root: Path) -> list[str]:
    """Validate one repository policy against its local lifecycle document."""
    errors: list[str] = []
    policy_file = repository_root / "repository-policy.yaml"
    lifecycle_doc = repository_root / "docs" / "REPOSITORY-LIFECYCLE.md"

    try:
        policy = yaml.safe_load(policy_file.read_text(encoding="utf-8")) or {}
    except Exception as error:
        return [f"Failed to parse repository-policy.yaml: {error}"]

    repository = policy.get("repository", {})
    lifecycle_class = repository.get("lifecycle_class")
    visibility = repository.get("visibility")
    if lifecycle_class not in VALID_CLASSES:
        errors.append(f"Invalid lifecycle_class '{lifecycle_class}'")

    role = policy.get("distribution", {}).get("role")
    if role not in VALID_ROLES:
        errors.append(f"Invalid distribution.role '{role}'")

    ci_authority = policy.get("authorities", {}).get("ci")
    if visibility == "public" and ci_authority != "github":
        errors.append(f"Public repository must have ci_authority 'github', found '{ci_authority}'")

    if visibility == "public" and policy.get("trust", {}).get("public_requires_private", False):
        errors.append("Public repository must not declare public_requires_private=True")

    if not lifecycle_doc.exists():
        errors.append("Missing docs/REPOSITORY-LIFECYCLE.md")
    else:
        lifecycle_text = lifecycle_doc.read_text(encoding="utf-8")
        if lifecycle_class not in lifecycle_text:
            errors.append(f"docs/REPOSITORY-LIFECYCLE.md does not match lifecycle_class '{lifecycle_class}'")
        if role not in lifecycle_text:
            errors.append(f"docs/REPOSITORY-LIFECYCLE.md does not match distribution.role '{role}'")
        if ci_authority not in lifecycle_text:
            errors.append(f"docs/REPOSITORY-LIFECYCLE.md does not match authorities.ci '{ci_authority}'")

    return errors


def main() -> int:
    repository_root = find_repository_root()
    print(f"Validating repository policy in: {repository_root}\n")
    errors = validate_repository_policy(repository_root)

    if errors:
        print(f"[ERROR] Found {len(errors)} repository policy error(s):")
        for error in errors:
            print(f"   {error}")
        return 1

    print("[SUCCESS] Repository policy and lifecycle document validated cleanly!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
