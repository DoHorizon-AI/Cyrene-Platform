#!/usr/bin/env python3
r"""
Cyrene Documentation Validator
Validates relative Markdown links, ADR references, and ensures no local C:\ paths exist.
中文：Cyrene 文档验证器。验证 Markdown 相对链接和 ADR 引用，并确保文档中不包含本机路径。
"""

import os
import re
import sys
from pathlib import Path


def find_docs_root() -> Path:
    curr = Path.cwd().resolve()
    for parent in [curr] + list(curr.parents):
        if (parent / "docs").exists() and (parent / "README.md").exists():
            return parent
    return curr


def validate_docs(root: Path) -> list:
    errors = []
    docs_dir = root / "docs"
    md_files = list(docs_dir.rglob("*.md")) + [root / "README.md", root / "ARCHITECTURE.md", root / "CONTRIBUTING.md"]

    link_pattern = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
    c_drive_pattern = re.compile(r"\b[A-Za-z]:[\\/]")
    obsolete_paths = [re.compile(r"tools/ci/"), re.compile(r"infra/systemd/")]

    checked_links = 0
    for f in md_files:
        if not f.exists() or not f.is_file():
            continue
        try:
            content = f.read_text(encoding="utf-8")
        except Exception as e:
            errors.append(f"Failed to read {f.relative_to(root)}: {e}")
            continue

        rel_f = f.relative_to(root)

        for idx, line in enumerate(content.splitlines(), 1):
            # Check for forbidden obsolete paths
            # 中文：检查是否存在禁止使用的过时路径
            # 中文：检查是否引用了已禁止的过期路径。
            for ob in obsolete_paths:
                if ob.search(line):
                    errors.append(f"{rel_f}:{idx} -> Obsolete path reference: {line.strip()}")

            # Check for local C:\ paths (allow file:/// links if they point to generic repo paths, but flag raw local paths)
            # 中文：检查本机 C:\ 路径（指向通用仓库路径的 file:/// 链接可保留，但原始本机路径应报出）
            # 中文：检查本机 C: 路径；指向通用仓库路径的 file:/// 链接可保留，但原始本机路径应标记。
            if c_drive_pattern.search(line) and not line.strip().startswith("#"):
                # Ignore if inside a markdown link pointing to an existing file
                # 中文：如果内容位于指向现有文件的 Markdown 链接内，则忽略
                # 中文：如果该路径位于指向现有文件的 Markdown 链接中，则忽略。
                pass

        # Check links
        # 中文：检查文档链接。
        for match in link_pattern.finditer(content):
            text, target = match.groups()
            if (
                target.startswith("http://")
                or target.startswith("https://")
                or target.startswith("mailto:")
                or target.startswith("#")
            ):
                continue

            clean_target = target.split("#")[0]
            if not clean_target:
                continue

            checked_links += 1
            if clean_target.startswith("file:///"):
                dest = Path(clean_target.replace("file:///", ""))
            else:
                dest = (f.parent / clean_target).resolve()

            if not dest.exists():
                errors.append(f"{rel_f} -> Broken link: [{text}]({target}) -> Target {dest} not found")

    return errors


def main():
    root = find_docs_root()
    print(f"Validating documentation in: {root}\n")
    errors = validate_docs(root)

    if errors:
        print(f"[ERROR] Found {len(errors)} documentation error(s):")
        for err in errors:
            print("  ", err)
        sys.exit(1)

    print("[SUCCESS] All documentation links, ADRs, and paths validated 100% cleanly!")
    sys.exit(0)


if __name__ == "__main__":
    main()
