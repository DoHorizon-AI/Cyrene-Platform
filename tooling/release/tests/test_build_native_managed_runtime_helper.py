"""The signed Runtime Maintenance archive owns the setup helper payload."""

from __future__ import annotations

import stat
from pathlib import Path

import pytest

pytest.importorskip("rfc8785", reason="release-builder tests require tooling/release/requirements.txt")

from tooling.release import build_native_components as builder
from tooling.release.component_artifacts import ComponentArtifactError


def test_managed_runtime_helper_is_scoped_to_maintenance_payload(tmp_path: Path) -> None:
    repository = tmp_path / "repository"
    source = repository / "tooling/runtime/cyrene_managed_runtime.py"
    source.parent.mkdir(parents=True)
    source.write_text("signed-helper-fixture\n")
    payload = tmp_path / "payload"
    payload.mkdir()

    builder._copy_managed_runtime_helper(repository, "cyrene-kernel", payload)
    assert not (payload / "share/cyrene-managed-runtime/cyrene_managed_runtime.py").exists()

    builder._copy_managed_runtime_helper(repository, "cyrene-runtime-maintenance", payload)
    copied = payload / "share/cyrene-managed-runtime/cyrene_managed_runtime.py"
    assert copied.read_text() == "signed-helper-fixture\n"
    assert stat.S_IMODE(copied.stat().st_mode) == 0o644


def test_managed_runtime_helper_rejects_symlink_source(tmp_path: Path) -> None:
    repository = tmp_path / "repository"
    source = repository / "tooling/runtime/cyrene_managed_runtime.py"
    source.parent.mkdir(parents=True)
    source.symlink_to(tmp_path / "elsewhere")

    with pytest.raises(ComponentArtifactError, match="not a regular file"):
        builder._copy_managed_runtime_helper(repository, "cyrene-runtime-maintenance", tmp_path / "payload")
