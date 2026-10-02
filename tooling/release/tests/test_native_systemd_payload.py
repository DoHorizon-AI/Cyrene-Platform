"""Native systemd unit payload binding and Worker-scope policy tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from tooling.release import build_native_components as native
from tooling.release.component_artifacts import ComponentArtifactError, _payload_files


REPOSITORY = Path(__file__).resolve().parents[3]


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
    assert (payload_root / copied).read_bytes() == (
        REPOSITORY / "infrastructure" / "systemd" / unit_name
    ).read_bytes()
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
