"""Package Runtime native target, catalog, ABI, and systemd producer checks.

cy-package-runtime 原生目标、目录授权、ABI 与 systemd 发布检查。
"""

from __future__ import annotations

from copy import deepcopy
import shlex
from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("rfc8785", reason="release-builder tests require tooling/release/requirements.txt")

from tooling.release import build_native_components as native
from tooling.release.component_artifacts import ComponentArtifactError, _payload_files


REPOSITORY = Path(__file__).resolve().parents[3]
UNIT_PATH = REPOSITORY / "infrastructure/systemd/cyrene-package-runtime.service"
EXPECTED_TARGETS = {
    "linux-ubuntu-22.04-x86_64-systemd": "2.35",
    "linux-ubuntu-24.04-x86_64-systemd": "2.39",
}


def _catalog(target_id: str) -> dict[str, object]:
    """Build one minimal catalog row with the exact published native target."""
    return {
        "schemaVersion": 1,
        "targets": [
            {
                "id": target_id,
                "target": deepcopy(native.TARGETS[target_id]),
                "hostSupport": "supported",
            }
        ],
        "components": [
            {
                "componentId": native.PACKAGE_RUNTIME_ID,
                "publisher": "DoHorizon-AI/Cyrene-Platform",
                "artifactKinds": ["native-binary"],
                "dependencies": [],
                "restart": {"unit": "cyrene-package-runtime.service"},
                "systemdUnit": "cyrene-package-runtime.service",
                "targets": [
                    {
                        "targetId": target_id,
                        "artifactKind": "native-binary",
                        "support": "supported",
                    }
                ],
            }
        ],
    }


@pytest.mark.parametrize("target_id,glibc", sorted(EXPECTED_TARGETS.items()))
def test_package_runtime_has_one_catalog_authorized_ubuntu_target(target_id: str, glibc: str) -> None:
    """Bind each Ubuntu profile to its exact glibc ABI and systemd unit."""
    component = _catalog(target_id)["components"][0]

    assert native.TARGETS[target_id]["abi"] == f"glibc-{glibc}"
    assert component["componentId"] == native.PACKAGE_RUNTIME_ID
    assert {
        "id": native.PACKAGE_RUNTIME_ID,
        "package": native.PACKAGE_RUNTIME_ID,
        "binary": native.PACKAGE_RUNTIME_ID,
    } in native.COMPONENTS
    assert native._require_package_runtime_catalog_target(_catalog(target_id), target_id) == component


def test_missing_package_runtime_catalog_entry_is_not_silently_skipped() -> None:
    """A green release must not omit the newly required native daemon."""
    catalog = _catalog("linux-ubuntu-22.04-x86_64-systemd")
    catalog["components"] = []

    with pytest.raises(ComponentArtifactError, match="declare cy-package-runtime exactly once"):
        native._require_package_runtime_catalog_target(catalog, "linux-ubuntu-22.04-x86_64-systemd")


def test_package_runtime_rejects_catalog_target_abi_drift() -> None:
    """The signed catalog target tuple must match the runner profile byte for byte."""
    target_id = "linux-ubuntu-22.04-x86_64-systemd"
    catalog = _catalog(target_id)
    catalog["targets"][0]["target"]["abi"] = "glibc-2.39"

    with pytest.raises(ComponentArtifactError, match="does not match native profile"):
        native._require_package_runtime_catalog_target(catalog, target_id)


def test_package_runtime_rejects_unsupported_or_wrong_unit_declarations() -> None:
    """The publisher cannot downgrade target support or redirect its service unit."""
    target_id = "linux-ubuntu-24.04-x86_64-systemd"
    catalog = _catalog(target_id)
    catalog["components"][0]["targets"][0]["support"] = "contract-only"
    with pytest.raises(ComponentArtifactError, match="must authorize cy-package-runtime"):
        native._require_package_runtime_catalog_target(catalog, target_id)

    catalog = _catalog(target_id)
    catalog["components"][0]["restart"]["unit"] = "other.service"
    with pytest.raises(ComponentArtifactError, match="must bind the exact unit"):
        native._require_package_runtime_catalog_target(catalog, target_id)


@pytest.mark.parametrize(
    "target_id,ubuntu_version,glibc_version",
    [
        ("linux-ubuntu-22.04-x86_64-systemd", "22.04", "2.35"),
        ("linux-ubuntu-24.04-x86_64-systemd", "24.04", "2.39"),
    ],
)
def test_build_host_must_match_exact_ubuntu_glibc_profile(
    target_id: str, ubuntu_version: str, glibc_version: str
) -> None:
    """Refuse to label artifacts built on a different distro or libc."""
    with (
        patch.object(
            native.platform,
            "freedesktop_os_release",
            return_value={"ID": "ubuntu", "VERSION_ID": ubuntu_version},
        ),
        patch.object(native.platform, "machine", return_value="x86_64"),
        patch.object(native.platform, "libc_ver", return_value=("glibc", glibc_version)),
    ):
        native._assert_build_host_matches_target(target_id)


def test_build_host_rejects_wrong_ubuntu_architecture_and_glibc() -> None:
    """Each build dimension fails closed when the runner differs from the catalog target."""
    target_id = "linux-ubuntu-22.04-x86_64-systemd"
    for os_release, machine, libc, message in (
        ({"ID": "debian", "VERSION_ID": "12"}, "x86_64", ("glibc", "2.35"), "requires ubuntu 22.04"),
        ({"ID": "ubuntu", "VERSION_ID": "24.04"}, "x86_64", ("glibc", "2.35"), "requires ubuntu 22.04"),
        ({"ID": "ubuntu", "VERSION_ID": "22.04"}, "aarch64", ("glibc", "2.35"), "requires x86_64"),
        ({"ID": "ubuntu", "VERSION_ID": "22.04"}, "x86_64", ("glibc", "2.39"), "requires glibc-2.35"),
    ):
        with (
            patch.object(native.platform, "freedesktop_os_release", return_value=os_release),
            patch.object(native.platform, "machine", return_value=machine),
            patch.object(native.platform, "libc_ver", return_value=libc),
            pytest.raises(ComponentArtifactError, match=message),
        ):
            native._assert_build_host_matches_target(target_id)


@pytest.mark.parametrize(
    "target_id,maximum",
    [
        ("linux-ubuntu-22.04-x86_64-systemd", "2.35"),
        ("linux-ubuntu-24.04-x86_64-systemd", "2.39"),
    ],
)
def test_binary_abi_accepts_only_x86_64_elf_within_target_glibc(target_id: str, maximum: str) -> None:
    """Inspect real ELF metadata before binding a binary to the manifest target."""
    header = "ELF Header:\n  Class: ELF64\n  Machine: Advanced Micro Devices X86-64\n"
    versions = f"Version needs section:\n  Name: GLIBC_{maximum} Flags: none Version: 1\n"
    with patch.object(native, "_run", side_effect=[header, versions]) as run:
        native._validate_native_binary_abi(Path("target/release/cy-package-runtime"), target_id, repository=REPOSITORY)

    assert run.call_count == 2


def test_binary_abi_rejects_newer_glibc_private_symbols_and_unknown_dynamic_abi() -> None:
    """Do not stamp a binary when its ELF requirements exceed the declared host ABI."""
    header = "ELF Header:\n  Class: ELF64\n  Machine: Advanced Micro Devices X86-64\n"
    with patch.object(native, "_run", side_effect=[header, "Name: GLIBC_2.36 Flags: none\n"]):
        with pytest.raises(ComponentArtifactError, match="exceeding target glibc-2.35"):
            native._validate_native_binary_abi(
                Path("target/release/cy-package-runtime"),
                "linux-ubuntu-22.04-x86_64-systemd",
                repository=REPOSITORY,
            )

    with patch.object(native, "_run", side_effect=[header, "GLIBC_PRIVATE"]):
        with pytest.raises(ComponentArtifactError, match="private GLIBC"):
            native._validate_native_binary_abi(
                Path("target/release/cy-package-runtime"),
                "linux-ubuntu-24.04-x86_64-systemd",
                repository=REPOSITORY,
            )

    with patch.object(
        native, "_run", side_effect=[header, "Version needs section: no recognized version\n", "NEEDED libc.so.6"]
    ):
        with pytest.raises(ComponentArtifactError, match="ABI could not be established"):
            native._validate_native_binary_abi(
                Path("target/release/cy-package-runtime"),
                "linux-ubuntu-24.04-x86_64-systemd",
                repository=REPOSITORY,
            )


def test_binary_abi_rejects_wrong_elf_architecture_and_old_relr_tag() -> None:
    """Reject architecture drift and GNU property tags unavailable on Ubuntu 22.04."""
    wrong_header = "ELF Header:\n  Class: ELF64\n  Machine: AArch64\n"
    with patch.object(native, "_run", return_value=wrong_header):
        with pytest.raises(ComponentArtifactError, match="not an x86_64 ELF64"):
            native._validate_native_binary_abi(
                Path("target/release/cy-package-runtime"),
                "linux-ubuntu-22.04-x86_64-systemd",
                repository=REPOSITORY,
            )

    header = "ELF Header:\n  Class: ELF64\n  Machine: Advanced Micro Devices X86-64\n"
    with patch.object(native, "_run", side_effect=[header, "Name: GLIBC_2.35\nGLIBC_ABI_DT_RELR"]):
        with pytest.raises(ComponentArtifactError, match="GLIBC_ABI_DT_RELR"):
            native._validate_native_binary_abi(
                Path("target/release/cy-package-runtime"),
                "linux-ubuntu-22.04-x86_64-systemd",
                repository=REPOSITORY,
            )


def test_package_runtime_systemd_payload_is_bound_to_the_nonroot_daemon() -> None:
    """Package the real UDS daemon unit with nonroot identity and control-group teardown."""
    unit = UNIT_PATH.read_text(encoding="utf-8")
    exec_line = next(line.partition("=")[2] for line in unit.splitlines() if line.startswith("ExecStart="))
    argv = shlex.split(exec_line)
    dependency_args = [argv[index + 1] for index, value in enumerate(argv[:-1]) if value == "--dependency-preparer-arg"]

    assert native.SYSTEMD_UNIT_COMPONENTS[native.PACKAGE_RUNTIME_ID] == "cyrene-package-runtime.service"
    assert native.PACKAGE_RUNTIME_ID in {component["id"] for component in native.COMPONENTS}
    assert argv[:4] == ["/usr/bin/cyrene", "component-run", "cy-package-runtime", "--"]
    assert argv[argv.index("--dependency-preparer") + 1] == "/usr/libexec/cyrene-plugin-python-preparer"
    assert argv[argv.index("--socket") + 1] == "/run/cyrene-package-runtime/control.sock"
    assert argv[argv.index("--source-policy") + 1] == "/etc/cyrene/runtime-package-sources.json"
    assert dependency_args == [
        "--uv",
        "/opt/cyrene/uv/0.12.21/uv",
        "--python",
        "/opt/cyrene/python/3.12.14/bin/python3.12",
    ]
    assert "User=cyrene\n" in unit and "Group=cyrene\n" in unit
    assert "KillMode=control-group\n" in unit
    assert "RuntimeDirectory=cyrene-package-runtime\n" in unit
    assert "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6\n" in unit
    assert "ProtectSystem=strict\n" in unit and "NoNewPrivileges=yes\n" in unit
    assert "ReadOnlyPaths=/etc/cyrene/runtime-package-sources.json" in unit
    assert "LoadCredential=" not in unit
    assert "operator.token" not in unit and "activity-sources.json" not in unit


def test_package_runtime_unit_is_copied_and_its_catalog_cannot_redirect_it(tmp_path: Path) -> None:
    """Bind only the root-owned source unit into the verified component archive."""
    component = _catalog("linux-ubuntu-22.04-x86_64-systemd")["components"][0]
    payload = tmp_path / "payload"
    payload.mkdir()

    relative = native._copy_systemd_unit_payload(REPOSITORY, component, payload)

    assert relative == "systemd/cyrene-package-runtime.service"
    assert (payload / relative).read_bytes() == UNIT_PATH.read_bytes()
    assert relative in _payload_files(payload)

    component["systemdUnit"] = "attacker.service"
    with pytest.raises(ComponentArtifactError, match="catalog unit does not match"):
        native._copy_systemd_unit_payload(REPOSITORY, component, tmp_path / "bad-payload")
