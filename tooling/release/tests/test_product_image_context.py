"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 test_product_image_context.py                                        │
│  Module: tooling.release.tests.test_product_image_context               │
│  Role: Guard exact-source Docker context composition and archive safety. │
│                                                                          │
│  模块职责：验证 OCI 上下文只包含固定 SHA，并拒绝不安全归档路径。        │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import json
import subprocess
import tarfile
from pathlib import Path

import pytest

from tooling.release import build_product_image_context as image_context


def _git(root: Path, *arguments: str) -> str:
    """Run Git in a fixture repository with deterministic author metadata."""
    result = subprocess.run(
        ["git", *arguments],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def _commit_repository(root: Path, files: dict[str, str], remote: str) -> str:
    """Create one clean fixture commit with the requested canonical origin."""
    root.mkdir(parents=True)
    _git(root, "init", "--quiet")
    _git(root, "config", "user.name", "Release Test")
    _git(root, "config", "user.email", "release-test@example.invalid")
    _git(root, "remote", "add", "origin", remote)
    for relative, content in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    _git(root, "add", ".")
    _git(root, "commit", "--quiet", "-m", "fixture")
    return _git(root, "rev-parse", "HEAD")


def test_build_context_uses_workflow_source_and_locked_sibling_shas(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Archive the Product at its workflow SHA and every sibling at lock SHA."""
    product_root = tmp_path / "product"
    product_name = "Cyrene-Catalyst"
    product_commit = _commit_repository(
        product_root,
        {"Dockerfile": "FROM scratch\nCOPY Cyrene-Platform/sdk /sdk\n"},
        f"https://github.com/DoHorizon-AI/{product_name}.git",
    )
    sibling_sha = "a" * 40
    workspace_root = tmp_path / "workspace"
    workspace_commit = _commit_repository(
        workspace_root,
        {
            "release-lock.json": json.dumps(
                {
                    "repositories": {
                        "Cyrene-Catalyst": "c" * 40,
                        "Cyrene-Platform": sibling_sha,
                        "Cyrene-Plugins-Official": "b" * 40,
                    }
                }
            )
        },
        "https://github.com/DoHorizon-AI/Cyrene-Workspace.git",
    )
    fetched: list[tuple[str, str]] = []

    def fake_archive(repository: str, commit: str, destination: Path, temporary_root: Path) -> None:
        fetched.append((repository, commit))
        destination.mkdir(parents=True)
        (destination / "pinned-source.txt").write_text(f"{repository}@{commit}\n", encoding="utf-8")

    monkeypatch.setattr(image_context, "_archive_commit", fake_archive)
    output = tmp_path / "context"
    result = image_context.build_context(
        product="catalyst",
        product_root=product_root,
        product_commit=product_commit,
        workspace_root=workspace_root,
        workspace_commit=workspace_commit,
        output=output,
    )

    assert fetched == [
        ("Cyrene-Platform", sibling_sha),
        ("Cyrene-Plugins-Official", "b" * 40),
    ]
    assert (output / result["dockerfile"]).is_file()
    assert result["repositories"][product_name] == product_commit
    assert result["repositories"]["Cyrene-Platform"] == sibling_sha
    saved_manifest = json.loads((output / "cyrene-build-context.json").read_text(encoding="utf-8"))
    assert saved_manifest["workspaceBuilderCommit"] == workspace_commit
    assert saved_manifest["releaseLockSha256"] == result["releaseLockSha256"]


def test_extract_archive_rejects_parent_traversal(tmp_path: Path) -> None:
    """Reject archive members that could write outside their context root."""
    archive_path = tmp_path / "unsafe.tar"
    with tarfile.open(archive_path, "w") as archive:
        member = tarfile.TarInfo("../escaped")
        member.size = 1
        import io

        archive.addfile(member, io.BytesIO(b"x"))

    with pytest.raises(image_context.ImageContextError, match="unsafe path"):
        image_context._extract_archive(archive_path, tmp_path / "output")
    assert not (tmp_path / "escaped").exists()
