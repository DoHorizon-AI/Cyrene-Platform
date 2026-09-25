#!/usr/bin/env python3
"""
Cyrene Release Tag Validator
Validates Semantic Versioning syntax, repository tag format, and ensures tag does not already exist.
中文:Cyrene 发布标签验证器。验证 Semantic Versioning 语法和仓库标签格式,并确保该标签尚不存在。
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

SEMVER_REGEX = re.compile(r'^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$')
COMPONENT_TAG_REGEX = re.compile(r'^[a-zA-Z0-9_-]+/v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9a-zA-Z.-]+))?$')

def validate_tag_syntax(tag: str, is_multi_component: bool = False) -> bool:
    if is_multi_component:
        return bool(COMPONENT_TAG_REGEX.match(tag))
    return bool(SEMVER_REGEX.match(tag))

def check_tag_existence(repo_path: Path, tag: str) -> bool:
    res = subprocess.run(["git", "-C", str(repo_path), "tag", "-l", tag], capture_output=True, text=True)
    return tag in res.stdout.split()

def main():
    parser = argparse.ArgumentParser(description="Cyrene Release Tag Validator")
    parser.add_argument("--repo-path", required=True, help="Path to repository")
    parser.add_argument("--tag", required=True, help="Tag string to validate")
    parser.add_argument("--multi-component", action="store_true", help="Allow component-namespaced tags (e.g. hf-analyzer/v0.3.0)")

    args = parser.parse_args()
    rp = Path(args.repo_path).resolve()

    print(f"Validating tag '{args.tag}' for repository: {rp}")

    if not validate_tag_syntax(args.tag, args.multi_component):
        print(f"[ERROR] Tag '{args.tag}' does not match expected SemVer tag format!")
        sys.exit(1)

    if rp.exists() and (rp / ".git").exists():
        if check_tag_existence(rp, args.tag):
            print(f"[ERROR] Tag '{args.tag}' already exists in {rp} (Tags are immutable and must not move!)")
            sys.exit(1)

    print(f"[SUCCESS] Tag '{args.tag}' is syntactically valid and available for release.")
    sys.exit(0)

if __name__ == "__main__":
    main()
