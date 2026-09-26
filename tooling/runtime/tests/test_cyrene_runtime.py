"""Runtime bootstrap state, identity and evidence regressions.

中文:Runtime bootstrap 状态、身份和证据的回归测试。
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

MODULE = Path(__file__).parents[1] / "cyrene_runtime.py"
SPEC = importlib.util.spec_from_file_location("cyrene_runtime", MODULE)
assert SPEC and SPEC.loader
runtime = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = runtime
SPEC.loader.exec_module(runtime)


def test_public_evidence_never_contains_private_runtime_paths(tmp_path: Path) -> None:
    private = str(tmp_path / "developer-private-runtime")
    manifest = {
        "profile": runtime.PROFILE,
        "runtimeMode": runtime.NATIVE_PROFILE,
        "status": "READY",
        "platformRevision": "a" * 40,
        "host": {"host": "Linux", "gpuRuntime": "NATIVE_LINUX_CUDA", "hardIsolation": True},
        "gpu": {"count": 1, "name": "NVIDIA GPU", "memoryMiB": 16384},
        "runtimeHome": private,
        "artifactRoot": private + "/artifacts",
        "components": {"kernel": {"status": "READY", "binary": private + "/bin/cyrene-kernel"}},
    }

    evidence = runtime._public_evidence(manifest)

    assert private not in json.dumps(evidence)
    assert evidence["components"] == {"kernel": "READY"}


def test_layout_rejects_filesystem_root() -> None:
    with pytest.raises(runtime.BootstrapFailure, match="bounded absolute path"):
        runtime.Layout.create(Path("/"))


def test_process_identity_requires_matching_proc_start_time(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(runtime, "_proc_start_time", lambda _pid: 200)

    assert runtime._alive({"pid": 12, "startTime": 200})
    assert not runtime._alive({"pid": 12, "startTime": 199})
    assert not runtime._alive({"pid": "12", "startTime": 200})


def test_wsl_requires_explicit_development_profile(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    args = runtime.parser().parse_args(["up", "--runtime-home", str(tmp_path)])
    monkeypatch.setattr(runtime, "_is_wsl", lambda: True)

    with pytest.raises(runtime.BootstrapFailure) as caught:
        runtime.up(args, runtime.Layout.create(tmp_path))

    assert caught.value.code == "WSL_PROFILE_REQUIRED"
