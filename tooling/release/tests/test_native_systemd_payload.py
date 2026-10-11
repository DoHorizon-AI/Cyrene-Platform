"""Native systemd unit payload binding and Worker-scope policy tests."""

from __future__ import annotations

import shlex
import subprocess
import sys
from pathlib import Path

import pytest

from tooling.release import build_native_components as native
from tooling.release.component_artifacts import ComponentArtifactError, _payload_files


REPOSITORY = Path(__file__).resolve().parents[3]
SHARED_RUNTIME_DIRECTORY_UNITS = {
    "cyrene-kernel.service",
    "cyrene-runtime-maintenance.service",
    "cyrene-linux-sys-adapter.service",
    "cyrene-nvidia-adapter.service",
    "cyrene-sandboxd.service",
}


def _catalog_components() -> dict[str, dict[str, object]]:
    rows = {
        component_id: {"componentId": component_id, "systemdUnit": unit_name}
        for component_id, unit_name in native.SYSTEMD_UNIT_COMPONENTS.items()
    }
    rows.update(
        {
            component_id: {"componentId": component_id, "systemdUnit": unit_name}
            for component_id, unit_name in native.WORKER_SCOPED_COMPONENT_UNITS.items()
        }
    )
    return rows


def test_each_native_component_has_a_host_unit_or_worker_policy() -> None:
    """Cover all built native targets without inventing a runtime-agent host service."""

    catalog_components = _catalog_components()
    native_ids = {component["id"] for component in native.COMPONENTS}
    assert native_ids == set(native.SYSTEMD_UNIT_COMPONENTS) | set(native.WORKER_SCOPED_COMPONENT_UNITS)

    for component_id in native.SYSTEMD_UNIT_COMPONENTS:
        component = catalog_components[component_id]
        assert component.get("systemdUnit") == native.SYSTEMD_UNIT_COMPONENTS[component_id]
        unit_path = REPOSITORY / "infrastructure" / "systemd" / native.SYSTEMD_UNIT_COMPONENTS[component_id]
        assert unit_path.is_file() and not unit_path.is_symlink()

    runtime_agent = catalog_components["cy-runtime-agent"]
    assert runtime_agent.get("systemdUnit") == native.WORKER_SCOPED_COMPONENT_UNITS["cy-runtime-agent"]
    assert not (REPOSITORY / "infrastructure" / "systemd" / "cy-runtime-agent.service").exists()


@pytest.mark.parametrize("component_id,unit_name", sorted(native.SYSTEMD_UNIT_COMPONENTS.items()))
def test_unit_is_copied_into_the_component_payload_and_bound_by_file_manifest(
    tmp_path: Path,
    component_id: str,
    unit_name: str,
) -> None:
    """The unit bytes are part of the immutable file map used by the manifest."""

    catalog_component = _catalog_components()[component_id]
    payload_root = tmp_path / "payload"
    payload_root.mkdir()

    copied = native._copy_systemd_unit_payload(REPOSITORY, catalog_component, payload_root)

    assert copied == f"systemd/{unit_name}"
    assert (payload_root / copied).read_bytes() == (REPOSITORY / "infrastructure" / "systemd" / unit_name).read_bytes()
    assert copied in _payload_files(payload_root)


def test_worker_runtime_agent_gets_no_host_unit_payload(tmp_path: Path) -> None:
    """Worker instances must be discovered by the runtime authority, not a fake host unit."""

    payload_root = tmp_path / "payload"
    payload_root.mkdir()

    copied = native._copy_systemd_unit_payload(
        REPOSITORY,
        _catalog_components()["cy-runtime-agent"],
        payload_root,
    )

    assert copied is None
    assert not (payload_root / "systemd").exists()


def test_all_services_sharing_cyrene_runtime_directory_preserve_it() -> None:
    """A service stop must not unlink sockets still owned by another live service."""

    shared_runtime_units: set[str] = set()
    for unit_path in sorted((REPOSITORY / "infrastructure" / "systemd").glob("*.service")):
        service_section = False
        runtime_directories: list[str] = []
        preserve_values: list[str] = []
        for raw_line in unit_path.read_text(encoding="utf-8").splitlines():
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("[") and line.endswith("]"):
                service_section = line == "[Service]"
                continue
            if not service_section:
                continue
            key, separator, value = line.partition("=")
            if not separator:
                continue
            if key == "RuntimeDirectory":
                runtime_directories.append(value)
            elif key == "RuntimeDirectoryPreserve":
                preserve_values.append(value)

        if "cyrene" not in runtime_directories:
            continue
        shared_runtime_units.add(unit_path.name)
        assert runtime_directories == ["cyrene"]
        assert preserve_values == ["yes"]

    assert shared_runtime_units == SHARED_RUNTIME_DIRECTORY_UNITS


def test_shared_runtime_directory_lifecycle_with_two_isolated_services() -> None:
    """Exercise real systemd cleanup using only uniquely named transient user units."""

    lifecycle_script = REPOSITORY / "tooling" / "release" / "tests" / "systemd_shared_runtime_directory_lifecycle.py"
    result = subprocess.run(
        [sys.executable, str(lifecycle_script)],
        capture_output=True,
        check=False,
        text=True,
        timeout=90,
    )
    if result.returncode == 77:
        pytest.skip(result.stdout.strip() or result.stderr.strip() or "systemd user manager unavailable")
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"


def test_catalog_cannot_redirect_native_unit_source(tmp_path: Path) -> None:
    """The trusted catalog unit and the source path must agree exactly."""

    component = dict(_catalog_components()["cyrene-runtime-maintenance"])
    component["systemdUnit"] = "attacker.service"
    payload_root = tmp_path / "payload"
    payload_root.mkdir()

    with pytest.raises(ComponentArtifactError, match="catalog unit does not match"):
        native._copy_systemd_unit_payload(REPOSITORY, component, payload_root)


def test_symlinked_unit_source_is_rejected(tmp_path: Path) -> None:
    """Unit payload sources may not escape the Platform repository."""

    source_root = tmp_path / "repository"
    unit_root = source_root / "infrastructure" / "systemd"
    unit_root.mkdir(parents=True)
    outside = tmp_path / "outside.service"
    outside.write_text("[Service]\nExecStart=/bin/false\n", encoding="utf-8")
    unit_name = native.SYSTEMD_UNIT_COMPONENTS["cyrene-runtime-maintenance"]
    (unit_root / unit_name).symlink_to(outside)
    payload_root = tmp_path / "payload"
    payload_root.mkdir()

    with pytest.raises(ComponentArtifactError, match="not a regular file"):
        native._copy_systemd_unit_payload(
            source_root,
            _catalog_components()["cyrene-runtime-maintenance"],
            payload_root,
        )


def test_kernel_state_directory_cannot_take_over_broker_acl_tree() -> None:
    """Keep Kernel StateDirectory ownership away from shared broker descendants."""

    kernel_unit = (REPOSITORY / "infrastructure" / "systemd" / "cyrene-kernel.service").read_text(encoding="utf-8")
    broker_unit = (REPOSITORY / "infrastructure" / "systemd" / "cyrene-runtime-maintenance.service").read_text(
        encoding="utf-8"
    )
    kernel_state_directories = [
        line.partition("=")[2] for line in kernel_unit.splitlines() if line.startswith("StateDirectory=")
    ]

    assert kernel_state_directories == []
    assert "SupplementaryGroups=cyrene-runtime-maintenance" in kernel_unit
    assert "ReadWritePaths=/run/cyrene /var/lib/cyrene" in kernel_unit
    assert "StateDirectory=cyrene/runtime-maintenance-private" in broker_unit
    assert "StateDirectoryMode=0700" in broker_unit
    assert "Group=cyrene" in broker_unit


def test_kernel_unit_peer_arguments_use_host_resolvable_identity_selectors() -> None:
    """Keep signed Kernel peer selectors aligned with native account names."""

    kernel_unit = (REPOSITORY / "infrastructure/systemd/cyrene-kernel.service").read_text(encoding="utf-8")
    exec_start = next(line.partition("=")[2] for line in kernel_unit.splitlines() if line.startswith("ExecStart="))
    command = shlex.split(exec_start)
    kernel_args = command[command.index("--") + 1 :]

    sandbox_uid = kernel_args[kernel_args.index("--sandbox-adapter-peer-uid") + 1]
    sandbox_gid = kernel_args[kernel_args.index("--sandbox-adapter-peer-gid") + 1]
    assert sandbox_uid == "root"
    assert sandbox_gid == "cyrene"
    for flag, expected in (
        ("--system-adapter-peer-uid", "linux-system=root"),
        ("--system-adapter-peer-gid", "linux-system=cyrene"),
        ("--hardware-adapter-peer-uid", "nvidia=root"),
        ("--hardware-adapter-peer-gid", "nvidia=cyrene"),
    ):
        assert kernel_args[kernel_args.index(flag) + 1] == expected


def test_native_adapter_units_select_kernel_account_and_cyrene_group() -> None:
    """Keep both ends of each strict UDS check on the same installed identities."""

    expected_units = (
        "cyrene-sandboxd.service",
        "cyrene-linux-sys-adapter.service",
        "cyrene-nvidia-adapter.service",
    )
    for unit_name in expected_units:
        unit = (REPOSITORY / "infrastructure/systemd" / unit_name).read_text(encoding="utf-8")
        exec_start = next(line.partition("=")[2] for line in unit.splitlines() if line.startswith("ExecStart="))
        command = shlex.split(exec_start)
        unit_args = command[command.index("--") + 1 :]

        assert unit_args[unit_args.index("--allowed-client-uid") + 1] == "cyrene-kernel"
        assert unit_args[unit_args.index("--allowed-client-gid") + 1] == "cyrene"
        assert "Group=cyrene" in unit

    kernel = (REPOSITORY / "infrastructure/systemd/cyrene-kernel.service").read_text(encoding="utf-8")
    assert "User=cyrene-kernel" in kernel
    assert "Group=cyrene" in kernel


def test_kernel_and_maintenance_broker_share_the_activity_catalog_path() -> None:
    """The signed units must consume the same root-managed activity catalog."""

    kernel_unit = (REPOSITORY / "infrastructure/systemd/cyrene-kernel.service").read_text(encoding="utf-8")
    broker_unit = (REPOSITORY / "infrastructure/systemd/cyrene-runtime-maintenance.service").read_text(encoding="utf-8")
    kernel_exec = next(line.partition("=")[2] for line in kernel_unit.splitlines() if line.startswith("ExecStart="))
    broker_exec = next(line.partition("=")[2] for line in broker_unit.splitlines() if line.startswith("ExecStart="))
    kernel_args = shlex.split(kernel_exec)
    broker_args = shlex.split(broker_exec)

    kernel_catalog = kernel_args[kernel_args.index("--activity-catalog") + 1]
    broker_catalog = broker_args[broker_args.index("--catalog") + 1]

    assert kernel_catalog == broker_catalog == "/var/lib/cyrene/runtime/activity-sources.json"
