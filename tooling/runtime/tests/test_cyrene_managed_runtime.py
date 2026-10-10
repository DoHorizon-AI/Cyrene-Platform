"""Focused safety tests for the formal managed-runtime setup helper."""

from __future__ import annotations

import json
import os
import shlex
import stat
from pathlib import Path
from types import SimpleNamespace

import pytest

from tooling.runtime import cyrene_managed_runtime as managed


def test_prepare_uses_live_ids_and_never_controls_units(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(managed.os, "geteuid", lambda: 0)
    ownership: list[tuple[str, int, int]] = []
    monkeypatch.setattr(managed.os, "chown", lambda path, uid, gid: ownership.append((str(path), uid, gid)))
    monkeypatch.setattr(managed, "_stage_kernel_account", lambda: 1843)
    monkeypatch.setattr(managed, "_account_ids", lambda: (1843, 2764, 2765))
    calls: list[list[str]] = []

    def forbidden_run(command: list[str], **_kwargs: object) -> SimpleNamespace:
        calls.append(command)
        raise AssertionError("prepare must not invoke systemctl or service control")

    monkeypatch.setattr(managed.subprocess, "run", forbidden_run)
    result = managed.prepare(tmp_path)

    assert result["status"] == "STAGED"
    assert result["kernelUid"] == 1843
    assert result["cyreneGid"] == 2764
    assert calls == []
    kernel = (tmp_path / "etc/systemd/system/cyrene-kernel.service.d/10-cyrene-managed-runtime.conf").read_text()
    exec_start = next(line.partition("=")[2] for line in kernel.splitlines() if line.startswith("ExecStart=/"))
    kernel_args = shlex.split(exec_start)
    kernel_args = kernel_args[kernel_args.index("--") + 1 :]
    sandbox_uid = kernel_args[kernel_args.index("--sandbox-adapter-peer-uid") + 1]
    sandbox_gid = kernel_args[kernel_args.index("--sandbox-adapter-peer-gid") + 1]
    assert sandbox_uid == "0"
    assert sandbox_gid == "2764"
    assert all(value.isdecimal() and int(value) <= 2**32 - 1 for value in (sandbox_uid, sandbox_gid))
    for flag, expected in (
        ("--system-adapter-peer-uid", "linux-system=0"),
        ("--system-adapter-peer-gid", "linux-system=2764"),
        ("--hardware-adapter-peer-uid", "nvidia=0"),
        ("--hardware-adapter-peer-gid", "nvidia=2764"),
    ):
        assert kernel_args[kernel_args.index(flag) + 1] == expected
    assert ownership and all(uid == 0 for _, uid, _ in ownership)
    for unit in ("cyrene-sandboxd", "cyrene-linux-sys-adapter", "cyrene-nvidia-adapter"):
        text = (tmp_path / f"etc/systemd/system/{unit}.service.d/10-cyrene-managed-runtime.conf").read_text()
        assert "--allowed-client-uid 1843" in text
        assert "--allowed-client-gid 2764" in text


def test_prepare_rejects_mismatched_existing_dropin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(managed.os, "geteuid", lambda: 0)
    monkeypatch.setattr(managed.os, "chown", lambda *_args: None)
    monkeypatch.setattr(managed, "_stage_kernel_account", lambda: 1843)
    monkeypatch.setattr(managed, "_account_ids", lambda: (1843, 2764, 2765))
    path = tmp_path / "etc/systemd/system/cyrene-kernel.service.d/10-cyrene-managed-runtime.conf"
    path.parent.mkdir(parents=True)
    path.write_text("[Service]\n# operator content\n")
    path.chmod(0o644)

    with pytest.raises(managed.ManagedRuntimeError, match="differs from managed content"):
        managed.prepare(tmp_path)


def test_existing_symlinked_dropin_is_rejected(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(managed.os, "geteuid", lambda: 0)
    monkeypatch.setattr(managed.os, "chown", lambda *_args: None)
    monkeypatch.setattr(managed, "_stage_kernel_account", lambda: 1843)
    monkeypatch.setattr(managed, "_account_ids", lambda: (1843, 2764, 2765))
    path = tmp_path / "etc/systemd/system/cyrene-kernel.service.d/10-cyrene-managed-runtime.conf"
    path.parent.mkdir(parents=True)
    path.symlink_to(tmp_path / "other")

    with pytest.raises(managed.ManagedRuntimeError, match="contains a symlink"):
        managed.prepare(tmp_path)


def _readiness(status: str = "READY") -> dict[str, object]:
    return {
        "status": status,
        "gate_generation": 12,
        "install_catalog_generation": 7,
        "active_task_count": 0,
        "active_tasks": [],
        "unknown_activity_sources": [],
        "active_worker_count": 0,
        "active_allocation_count": 0,
        "blocker_codes": [],
    }


def _identity_fixture() -> dict[str, object]:
    return {
        "schemaVersion": 1,
        "units": {
            unit: {
                "unitFile": f"/usr/lib/systemd/system/{unit}",
                "unitSha256": "1" * 64,
                "binaryPath": f"/usr/lib/cyrene/components/{unit}/releases/test/bin/runtime",
                "binarySha256": "2" * 64,
            }
            for unit in managed.UNITS.values()
        },
    }


@pytest.mark.parametrize("status", ["UNKNOWN", "UNIMPLEMENTED", "IDLE_RUNTIME_REQUIRES_UNLOAD"])
def test_unknown_or_unsupported_readiness_cannot_project_ready(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, status: str
) -> None:
    monkeypatch.setattr(managed.os, "geteuid", lambda: 0)
    monkeypatch.setattr(managed, "_account_ids", lambda: (2001, 3001, 3002))
    monkeypatch.setattr(managed.pwd, "getpwnam", lambda _name: SimpleNamespace(pw_uid=os.getuid()))
    monkeypatch.setattr(managed, "_verify_managed_dropins", lambda *_args: None)
    monkeypatch.setattr(
        managed,
        "_load_trusted_identity",
        lambda _path: _identity_fixture(),
    )
    monkeypatch.setattr(
        managed,
        "_verify_active_unit",
        lambda unit, *_args: (22, 2001 if unit == managed.UNITS["kernel"] else 0, 3001),
    )
    monkeypatch.setattr(managed, "_verify_kernel_peer", lambda *_args: None)
    monkeypatch.setattr(managed, "_broker_readiness", lambda *_args: _readiness(status))
    manifest = tmp_path / "etc/cyrene/runtime/platform.json"

    with pytest.raises(managed.ManagedRuntimeError, match="readiness is"):
        managed.observe(tmp_path / "identity.json", tmp_path)
    assert not manifest.exists()


def test_missing_or_mismatched_signing_key_is_rejected(tmp_path: Path) -> None:
    key = tmp_path / "installer.key"
    key.write_bytes(b"short")
    key.chmod(0o600)

    with pytest.raises(managed.ManagedRuntimeError, match="32-byte cyrene-owned"):
        managed._ensure_signing_key(key, os.getuid(), os.getgid())


def test_existing_signing_key_is_preserved(tmp_path: Path) -> None:
    key = tmp_path / "installer.key"
    original = bytes(range(32))
    key.write_bytes(original)
    key.chmod(0o600)

    managed._ensure_signing_key(key, os.getuid(), os.getgid())

    assert key.read_bytes() == original


def test_broker_request_uses_operator_readiness_and_live_catalog(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    catalog_path = tmp_path / "activity-sources.json"
    catalog_path.write_text(json.dumps({
        "schema_version": 1,
        "generation": 7,
        "sources": [{"source_id": "product-alpha"}, {"source_id": "product-beta"}],
    }))
    broker_binary = tmp_path / "usr/lib/cyrene/components/cyrene-runtime-maintenance/releases/1/bin/cyrene-runtime-maintenance"
    broker_binary.parent.mkdir(parents=True)
    broker_binary.write_bytes(b"signed broker fixture")
    broker_binary.chmod(0o755)
    captured: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs: object) -> SimpleNamespace:
        captured["command"] = command
        captured["input"] = kwargs["input"]
        request = json.loads(str(kwargs["input"]))
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps({"request_id": request["request_id"], "result": _readiness()}),
        )

    monkeypatch.setattr(managed.subprocess, "run", fake_run)
    result = managed._broker_readiness(
        tmp_path,
        catalog_path,
        Path("/run/cyrene/runtime-maintenance.sock"),
        Path("/usr/lib/cyrene/components/cyrene-runtime-maintenance/releases/1/bin/cyrene-runtime-maintenance"),
        managed._digest(broker_binary),
    )
    request = json.loads(str(captured["input"]))
    assert captured["command"] == [
        str(broker_binary),
        "request", "--socket", str(tmp_path / "run/cyrene/runtime-maintenance.sock"), "--operator",
    ]
    assert request["method"] == "GetUpdateReadiness"
    assert request["auth"] == {}
    assert request["params"] == {
        "target_kind": "CORE_RUNTIME",
        "requires_restart": True,
        "expected_catalog_generation": 7,
        "expected_activity_sources": ["product-alpha", "product-beta"],
    }
    assert result["status"] == "READY"


def test_broker_unimplemented_readiness_is_blocked(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    catalog_path = tmp_path / "activity-sources.json"
    catalog_path.write_text(json.dumps({"schema_version": 1, "generation": 7, "sources": []}))
    broker_binary = tmp_path / "usr/lib/cyrene/components/cyrene-runtime-maintenance/releases/1/bin/cyrene-runtime-maintenance"
    broker_binary.parent.mkdir(parents=True)
    broker_binary.write_bytes(b"signed broker fixture")
    broker_binary.chmod(0o755)
    def unknown_run(_command: list[str], **kwargs: object) -> SimpleNamespace:
        request = json.loads(str(kwargs["input"]))
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps({"request_id": request["request_id"], "result": _readiness("UNKNOWN")}),
        )

    monkeypatch.setattr(managed.subprocess, "run", unknown_run)

    with pytest.raises(managed.ManagedRuntimeError, match="Kernel readiness is UNKNOWN"):
        managed._broker_readiness(
            tmp_path,
            catalog_path,
            Path("/run/cyrene/runtime-maintenance.sock"),
            Path("/usr/lib/cyrene/components/cyrene-runtime-maintenance/releases/1/bin/cyrene-runtime-maintenance"),
            managed._digest(broker_binary),
        )


def test_systemctl_named_properties_accept_real_world_out_of_order_output() -> None:
    output = (
        "MainPID=4321\n"
        "ActiveState=active\n"
        "FragmentPath=/usr/lib/systemd/system/cyrene-kernel.service\n"
        "DropInPaths=/etc/systemd/system/cyrene-kernel.service.d/10-cyrene-managed-runtime.conf\n"
    )

    parsed = managed._parse_systemd_show(output, "cyrene-kernel.service")

    assert parsed["ActiveState"] == "active"
    assert parsed["MainPID"] == "4321"
    assert parsed["FragmentPath"] == "/usr/lib/systemd/system/cyrene-kernel.service"


def test_projection_matches_yield_manifest_contract(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(managed.os, "geteuid", lambda: 0)
    ownership: list[tuple[str, int, int]] = []
    monkeypatch.setattr(managed.os, "chown", lambda path, uid, gid: ownership.append((str(path), uid, gid)))
    monkeypatch.setattr(
        managed,
        "_load_trusted_identity",
        lambda _path: _identity_fixture(),
    )
    monkeypatch.setattr(managed, "_verify_managed_dropins", lambda *_args: None)
    monkeypatch.setattr(
        managed,
        "_verify_active_unit",
        lambda unit, *_args: (24, 2001 if unit == managed.UNITS["kernel"] else 0, 3001),
    )
    monkeypatch.setattr(managed, "_verify_kernel_peer", lambda *_args: None)
    monkeypatch.setattr(managed, "_broker_readiness", lambda *_args: _readiness("ACTIVE_TASKS"))
    monkeypatch.setattr(managed, "_account_ids", lambda: (2001, 3001, 3002))
    monkeypatch.setattr(managed.pwd, "getpwnam", lambda _name: SimpleNamespace(pw_uid=os.getuid()))

    result = managed.observe(tmp_path / "trusted.json", tmp_path)
    manifest_path = tmp_path / "etc/cyrene/runtime/platform.json"
    projection = json.loads(manifest_path.read_text())
    assert result["status"] == "READY"
    assert projection == {
        "schemaVersion": 1,
        "profile": "CYRENE_PLATFORM_RUNTIME_V1_LOCAL_GPU",
        "status": "READY",
        "artifactRoot": "/var/lib/cyrene/artifacts",
        "installationsRoot": "/var/lib/cyrene/installations",
        "signingKeyFile": "/etc/cyrene/runtime/installer.key",
        "kernel": {"socket": "/run/cyrene/kernel.sock"},
        "components": {name: {"status": "READY"} for name in managed.COMPONENT_UNITS},
    }
    assert stat.S_IMODE(manifest_path.stat().st_mode) == 0o640
    assert any(path.startswith(str(manifest_path.parent / ".platform.json.")) and uid == 0 and gid == 3001
               for path, uid, gid in ownership)
    key = tmp_path / "etc/cyrene/runtime/installer.key"
    assert key.stat().st_size == 32
    assert stat.S_IMODE(key.stat().st_mode) == 0o600
    assert key.stat().st_uid == os.getuid()
    assert (str(key), os.getuid(), 0) in ownership
    runtime_config = tmp_path / "etc/cyrene/runtime"
    assert stat.S_IMODE(runtime_config.stat().st_mode) == 0o750
    assert (str(runtime_config), 0, 3001) in ownership


def test_existing_runtime_path_with_wrong_group_is_not_repaired(tmp_path: Path) -> None:
    artifact_root = tmp_path / "artifacts"
    artifact_root.mkdir()
    artifact_root.chmod(0o700)

    with pytest.raises(managed.ManagedRuntimeError, match="lacks cyrene group access"):
        managed._ensure_shared_directory(artifact_root, os.getgid() + 1)
