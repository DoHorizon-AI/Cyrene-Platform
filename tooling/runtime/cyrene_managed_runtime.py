#!/usr/bin/env python3
"""Prepare formal Linux runtime identities and project observed readiness.

This helper is intended to be invoked only after the installer has verified
and source-bound it to the signed native maintenance component. ``prepare``
stages missing account/configuration prerequisites only. ``observe`` writes a
Product-consumable manifest only after signed identity evidence, active unit
processes, the Kernel UDS peer, and authoritative broker readiness agree.
"""

from __future__ import annotations

import argparse
import grp
import hashlib
import json
import os
import pwd
import secrets
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any, Callable

PROFILE = "CYRENE_PLATFORM_RUNTIME_V1_LOCAL_GPU"
UNITS = {
    "kernel": "cyrene-kernel.service",
    "sandboxd": "cyrene-sandboxd.service",
    "systemAdapter": "cyrene-linux-sys-adapter.service",
    "nvidiaAdapter": "cyrene-nvidia-adapter.service",
    "runtimeMaintenance": "cyrene-runtime-maintenance.service",
}
COMPONENT_UNITS = {key: value for key, value in UNITS.items() if key != "runtimeMaintenance"}
DEFAULT_PATHS = {
    "manifest": "/etc/cyrene/runtime/platform.json",
    "key": "/etc/cyrene/runtime/installer.key",
    "catalog": "/var/lib/cyrene/runtime/activity-sources.json",
    "brokerSocket": "/run/cyrene/runtime-maintenance.sock",
    "kernelSocket": "/run/cyrene/kernel.sock",
    "artifactRoot": "/var/lib/cyrene/artifacts",
    "installationsRoot": "/var/lib/cyrene/installations",
}
DROPINS = {
    "cyrene-kernel.service": (
        "[Service]\n"
        "ExecStart=\n"
        "ExecStart=/usr/bin/cyrene component-run cyrene-kernel -- --socket /run/cyrene/kernel.sock "
        "--sandbox-adapter sandboxd=/run/cyrene/sandboxd.sock "
        "--sandbox-adapter-peer-uid sandboxd=0 --sandbox-adapter-peer-gid sandboxd={cyrene_gid} "
        "--system-adapter linux-system=/run/cyrene/linux-sys-adapter.sock "
        "--system-adapter-peer-uid linux-system=0 --system-adapter-peer-gid linux-system={cyrene_gid} "
        "--hardware-adapter nvidia=/run/cyrene/nvidia-adapter.sock "
        "--hardware-adapter-peer-uid nvidia=0 --hardware-adapter-peer-gid nvidia={cyrene_gid} "
        "--runtime-journal /var/lib/cyrene/runtime/journal.jsonl\n"
    ),
    "cyrene-sandboxd.service": (
        "[Service]\nExecStart=\n"
        "ExecStart=/usr/bin/cyrene component-run cyrene-sandboxd -- --adapter-id sandboxd "
        "--socket /run/cyrene/sandboxd.sock --allowed-client-uid {kernel_uid} "
        "--allowed-client-gid {cyrene_gid}\n"
    ),
    "cyrene-linux-sys-adapter.service": (
        "[Service]\nExecStart=\n"
        "ExecStart=/usr/bin/cyrene component-run cyrene-linux-sys-adapter -- "
        "--socket /run/cyrene/linux-sys-adapter.sock --adapter-id linux-system "
        "--allowed-client-uid {kernel_uid} --allowed-client-gid {cyrene_gid}\n"
    ),
    "cyrene-nvidia-adapter.service": (
        "[Service]\nExecStart=\n"
        "ExecStart=/usr/bin/cyrene component-run cyrene-nvidia-adapter -- "
        "--socket /run/cyrene/nvidia-adapter.sock --allowed-client-uid {kernel_uid} "
        "--allowed-client-gid {cyrene_gid}\n"
    ),
}


class ManagedRuntimeError(RuntimeError):
    """Fail-closed setup or observation error with a concise operator detail."""


def _account_ids() -> tuple[int, int, int]:
    """Return the Kernel UID and the actual cyrene and maintenance GIDs."""

    try:
        cyrene_gid = grp.getgrnam("cyrene").gr_gid
        maintenance_gid = grp.getgrnam("cyrene-runtime-maintenance").gr_gid
    except KeyError as error:
        raise ManagedRuntimeError(f"required group is missing: {error}") from error
    try:
        kernel_uid = pwd.getpwnam("cyrene-kernel").pw_uid
    except KeyError:
        kernel_uid = -1
    return kernel_uid, cyrene_gid, maintenance_gid


def _stage_kernel_account() -> int:
    """Create only a missing Kernel service account; reject existing drift."""

    try:
        account = pwd.getpwnam("cyrene-kernel")
    except KeyError:
        try:
            cyrene_gid = grp.getgrnam("cyrene").gr_gid
            maintenance_gid = grp.getgrnam("cyrene-runtime-maintenance").gr_gid
        except KeyError as error:
            raise ManagedRuntimeError(f"required group is missing: {error}") from error
        subprocess.run(
            [
                "useradd", "--system", "--no-create-home", "--gid", str(cyrene_gid),
                "--groups", str(maintenance_gid), "--shell", "/usr/sbin/nologin", "cyrene-kernel",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        try:
            account = pwd.getpwnam("cyrene-kernel")
        except KeyError as error:
            raise ManagedRuntimeError("useradd completed without creating cyrene-kernel") from error
    cyrene_gid = grp.getgrnam("cyrene").gr_gid
    if account.pw_gid != cyrene_gid:
        raise ManagedRuntimeError("existing cyrene-kernel primary group does not match cyrene")
    return account.pw_uid


def _safe_existing_file(path: Path) -> bytes | None:
    """Read one regular non-symlink file or report it absent."""

    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(metadata.st_mode):
        raise ManagedRuntimeError(f"existing path is not a regular file: {path}")
    return path.read_bytes()


def _reject_symlink_components(path: Path) -> None:
    """Reject a symlink at any existing component before following a path."""

    for component in reversed((path, *path.parents)):
        try:
            metadata = component.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(metadata.st_mode):
            raise ManagedRuntimeError(f"managed runtime path contains a symlink: {component}")


def _write_dropin(path: Path, content: str) -> None:
    """Install an exact root-owned drop-in, preserving and rejecting drift."""

    _reject_symlink_components(path)
    existing = _safe_existing_file(path)
    encoded = content.encode("utf-8")
    if existing is not None:
        metadata = path.stat()
        if existing != encoded or metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o644:
            raise ManagedRuntimeError(f"existing systemd drop-in differs from managed content: {path}")
        return
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    if path.parent.is_symlink():
        raise ManagedRuntimeError(f"systemd drop-in directory is a symlink: {path.parent}")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, "wb") as stream:
        stream.write(encoded)
        stream.flush()
        os.fsync(stream.fileno())
    os.chown(path, 0, 0)
    os.chmod(path, 0o644)


def prepare(root: Path = Path("/")) -> dict[str, Any]:
    """Stage missing identity and dynamic peer-credential overrides only.

    No service is enabled, started, stopped, or restarted, and no runtime
    pointer or component activation state is changed.
    """

    if os.geteuid() != 0:
        raise ManagedRuntimeError("prepare requires root")
    kernel_uid = _stage_kernel_account()
    _, cyrene_gid, _ = _account_ids()
    staged: list[str] = []
    for unit, template in DROPINS.items():
        path = root / "etc/systemd/system" / f"{unit}.d" / "10-cyrene-managed-runtime.conf"
        _write_dropin(path, template.format(kernel_uid=kernel_uid, cyrene_gid=cyrene_gid))
        staged.append(str(path))
    return {"status": "STAGED", "kernelUid": kernel_uid, "cyreneGid": cyrene_gid, "dropins": staged}


def _digest(path: Path) -> str:
    """Return a lowercase SHA-256 digest for one regular executable/unit."""

    metadata = path.stat()
    if not stat.S_ISREG(metadata.st_mode):
        raise ManagedRuntimeError(f"identity path is not a regular file: {path}")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load_trusted_identity(path: Path) -> dict[str, Any]:
    """Validate the wrapper's signed-source-bound unit and executable hashes."""

    _reject_symlink_components(path)
    try:
        raw = path.read_bytes()
        if len(raw) > 64 * 1024:
            raise ManagedRuntimeError("trusted identity evidence exceeds its size limit")
        document = json.loads(raw.decode("utf-8", "strict"))
    except (OSError, json.JSONDecodeError) as error:
        raise ManagedRuntimeError(f"trusted identity evidence is unreadable: {error}") from error
    if not isinstance(document, dict) or document.get("schemaVersion") != 1:
        raise ManagedRuntimeError("trusted identity evidence has an unsupported schema")
    units = document.get("units")
    if not isinstance(units, dict) or set(units) != set(UNITS.values()):
        raise ManagedRuntimeError("trusted identity evidence must cover all formal runtime units")
    for unit, evidence in units.items():
        if not isinstance(evidence, dict):
            raise ManagedRuntimeError(f"trusted identity evidence is malformed for {unit}")
        for key in ("unitFile", "unitSha256", "binaryPath", "binarySha256"):
            if not isinstance(evidence.get(key), str) or not evidence[key]:
                raise ManagedRuntimeError(f"trusted identity evidence is missing {key} for {unit}")
        for key in ("unitFile", "binaryPath"):
            if not Path(evidence[key]).is_absolute():
                raise ManagedRuntimeError(f"trusted identity evidence path must be absolute for {unit}")
        for key in ("unitSha256", "binarySha256"):
            if len(evidence[key]) != 64 or any(character not in "0123456789abcdef" for character in evidence[key]):
                raise ManagedRuntimeError(f"trusted identity evidence has an invalid {key} for {unit}")
    return document


def _verify_active_unit(
    unit: str, evidence: dict[str, Any], root: Path, kernel_uid: int, cyrene_gid: int
) -> tuple[int, int, int, set[int]]:
    """Match the live systemd PID, loaded unit bytes, and executable bytes."""

    result = subprocess.run(
        [
            "systemctl", "show", unit, "--property=ActiveState", "--property=MainPID",
            "--property=FragmentPath", "--property=DropInPaths",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    properties = _parse_systemd_show(result.stdout, unit)
    main_pid = properties.get("MainPID", "")
    if properties.get("ActiveState") != "active" or not main_pid.isdigit() or int(main_pid) < 1:
        raise ManagedRuntimeError(f"formal unit is not active with a MainPID: {unit}")
    pid = int(main_pid)
    unit_file = root / evidence["unitFile"].lstrip("/")
    if properties.get("FragmentPath") != evidence["unitFile"]:
        raise ManagedRuntimeError(f"active systemd fragment does not match signed unit identity: {unit}")
    dropins = (
        [str(root / "etc/systemd/system" / f"{unit}.d" / "10-cyrene-managed-runtime.conf")]
        if unit in DROPINS
        else []
    )
    if properties.get("DropInPaths", "").split() != dropins:
        raise ManagedRuntimeError(f"active systemd drop-in set does not match the managed identity: {unit}")
    binary_path = root / evidence["binaryPath"].lstrip("/")
    if _digest(unit_file) != evidence["unitSha256"] or _digest(binary_path) != evidence["binarySha256"]:
        raise ManagedRuntimeError(f"signed unit or binary identity changed: {unit}")
    proc_exe = root / f"proc/{pid}/exe"
    try:
        observed_exe = os.readlink(proc_exe)
        observed_path = Path(observed_exe.removesuffix(" (deleted)"))
        if observed_path != Path(evidence["binaryPath"]):
            raise ManagedRuntimeError(f"active process executable does not match signed binary: {unit}")
        if _digest(proc_exe) != evidence["binarySha256"]:
            raise ManagedRuntimeError(f"active process bytes do not match signed binary: {unit}")
        status = (root / f"proc/{pid}/status").read_text(encoding="utf-8")
        command_line = (root / f"proc/{pid}/cmdline").read_bytes().decode("utf-8", "strict").split("\0")
    except OSError as error:
        raise ManagedRuntimeError(f"cannot observe active unit process: {unit}") from error
    uid_line = next((line for line in status.splitlines() if line.startswith("Uid:")), None)
    gid_line = next((line for line in status.splitlines() if line.startswith("Gid:")), None)
    groups_line = next((line for line in status.splitlines() if line.startswith("Groups:")), None)
    if uid_line is None or gid_line is None or groups_line is None:
        raise ManagedRuntimeError(f"active unit process credentials are unavailable: {unit}")
    required_arguments = {
        UNITS["kernel"]: (
            "--sandbox-adapter-peer-uid", "sandboxd=0", "--sandbox-adapter-peer-gid", f"sandboxd={cyrene_gid}",
            "--system-adapter-peer-uid", "linux-system=0", "--system-adapter-peer-gid", f"linux-system={cyrene_gid}",
            "--hardware-adapter-peer-uid", "nvidia=0", "--hardware-adapter-peer-gid", f"nvidia={cyrene_gid}",
        ),
        UNITS["sandboxd"]: ("--allowed-client-uid", str(kernel_uid), "--allowed-client-gid", str(cyrene_gid)),
        UNITS["systemAdapter"]: ("--allowed-client-uid", str(kernel_uid), "--allowed-client-gid", str(cyrene_gid)),
        UNITS["nvidiaAdapter"]: ("--allowed-client-uid", str(kernel_uid), "--allowed-client-gid", str(cyrene_gid)),
    }.get(unit, ())
    if required_arguments and not _contains_subsequence(command_line, required_arguments):
        raise ManagedRuntimeError(f"active process peer credentials do not match the prepared drop-in: {unit}")
    groups = {int(value) for value in groups_line.split()[1:]}
    if unit in {UNITS["kernel"], UNITS["runtimeMaintenance"]} and _maintenance_group_missing(groups):
        raise ManagedRuntimeError(f"active process lacks cyrene-runtime-maintenance membership: {unit}")
    return pid, int(uid_line.split()[1]), int(gid_line.split()[1]), groups


def _parse_systemd_show(output: str, unit: str) -> dict[str, str]:
    """Parse named systemctl properties without relying on output ordering."""

    properties: dict[str, str] = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        if not separator or not key or key in properties:
            raise ManagedRuntimeError(f"systemd returned malformed properties for {unit}")
        properties[key] = value
    required = {"ActiveState", "MainPID", "FragmentPath", "DropInPaths"}
    if not required.issubset(properties):
        raise ManagedRuntimeError(f"systemd omitted required properties for {unit}")
    return properties


def _contains_subsequence(arguments: list[str], expected: tuple[str, ...]) -> bool:
    """Check required argv tokens in their declared order."""

    cursor = 0
    for argument in arguments:
        if cursor < len(expected) and argument == expected[cursor]:
            cursor += 1
    return cursor == len(expected)


def _maintenance_group_missing(groups: set[int]) -> bool:
    """Resolve whether the Kernel/broker supplemental runtime group is absent."""

    maintenance_gid = grp.getgrnam("cyrene-runtime-maintenance").gr_gid
    return maintenance_gid not in groups


def _verify_managed_dropins(root: Path, kernel_uid: int, cyrene_gid: int) -> None:
    """Require exact dynamic peer credentials in all four loaded drop-ins."""

    for unit, template in DROPINS.items():
        path = root / "etc/systemd/system" / f"{unit}.d" / "10-cyrene-managed-runtime.conf"
        _reject_symlink_components(path)
        actual = _safe_existing_file(path)
        expected = template.format(kernel_uid=kernel_uid, cyrene_gid=cyrene_gid).encode("utf-8")
        metadata = path.stat() if actual is not None else None
        if (
            actual != expected
            or metadata is None
            or metadata.st_uid != 0
            or metadata.st_gid != 0
            or stat.S_IMODE(metadata.st_mode) != 0o644
        ):
            raise ManagedRuntimeError(f"managed peer-credential drop-in is missing or changed: {unit}")


def _verify_kernel_peer(path: Path, expected: tuple[int, int, int]) -> None:
    """Connect read-only to the Kernel UDS and verify its observed peer PID."""

    _reject_symlink_components(path)
    metadata = path.lstat()
    if not stat.S_ISSOCK(metadata.st_mode):
        raise ManagedRuntimeError("Kernel UDS path is not a Unix socket")
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        client.settimeout(3.0)
        client.connect(str(path))
        peer_pid, peer_uid, peer_gid = struct.unpack("3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    except OSError as error:
        raise ManagedRuntimeError(f"Kernel UDS peer observation failed: {error}") from error
    finally:
        client.close()
    if (peer_pid, peer_uid, peer_gid) != expected:
        raise ManagedRuntimeError("Kernel UDS peer does not match the signed active Kernel unit")


def _catalog_expectations(path: Path) -> tuple[int, list[str]]:
    """Read the root-managed broker catalog generation and sorted sources."""

    _reject_symlink_components(path)
    try:
        raw = path.read_bytes()
        if len(raw) > 1024 * 1024:
            raise ManagedRuntimeError("activity catalog exceeds its size limit")
        catalog = json.loads(raw.decode("utf-8", "strict"))
    except (OSError, json.JSONDecodeError) as error:
        raise ManagedRuntimeError(f"activity catalog is unreadable: {error}") from error
    if not isinstance(catalog, dict) or catalog.get("schema_version") != 1:
        raise ManagedRuntimeError("activity catalog schema is unsupported")
    generation, entries = catalog.get("generation"), catalog.get("sources")
    if not isinstance(generation, int) or isinstance(generation, bool) or generation < 1 or not isinstance(entries, list):
        raise ManagedRuntimeError("activity catalog generation or sources are invalid")
    ids = [entry.get("source_id") for entry in entries if isinstance(entry, dict)]
    if len(ids) != len(entries) or any(not isinstance(value, str) or not value for value in ids):
        raise ManagedRuntimeError("activity catalog source records are invalid")
    if ids != sorted(ids) or len(ids) != len(set(ids)):
        raise ManagedRuntimeError("activity catalog source IDs must be unique and sorted")
    return generation, ids


def _validate_readiness(result: dict[str, Any], generation: int | None = None) -> dict[str, Any]:
    """Require a complete supported broker result before any READY projection."""

    # ACTIVE_TASKS is a supported BUSY observation; it is not proof of idle GPU
    # or hard isolation. Product and Kernel retain those separate fences.
    status = result.get("status")
    if status not in {"READY", "ACTIVE_TASKS"}:
        raise ManagedRuntimeError(f"authoritative Kernel readiness is {status or 'UNKNOWN'}")
    count_fields = ("active_task_count", "active_worker_count", "active_allocation_count", "gate_generation", "install_catalog_generation")
    if any(not isinstance(result.get(field), int) or isinstance(result.get(field), bool) or result[field] < 0 for field in count_fields):
        raise ManagedRuntimeError("broker readiness is missing supported non-UNKNOWN counters")
    if generation is not None and result["install_catalog_generation"] != generation:
        raise ManagedRuntimeError("broker readiness catalog generation does not match the installed catalog")
    if result.get("unknown_activity_sources") != []:
        raise ManagedRuntimeError("broker readiness reports unknown activity sources")
    active_tasks = result.get("active_tasks")
    if not isinstance(active_tasks, list) or len(active_tasks) != result["active_task_count"]:
        raise ManagedRuntimeError("broker readiness task evidence is incomplete")
    blockers = result.get("blocker_codes")
    if not isinstance(blockers, list) or any(not isinstance(code, str) for code in blockers):
        raise ManagedRuntimeError("broker readiness blocker evidence is malformed")
    return result


def _broker_readiness(
    root: Path,
    catalog_path: Path,
    broker_socket: Path,
    broker_binary: Path,
    broker_binary_sha256: str,
) -> dict[str, Any]:
    """Ask the authenticated broker for its authoritative core readiness."""

    generation, sources = _catalog_expectations(catalog_path)
    request_id = "cyrene-managed-runtime-" + uuid.uuid4().hex
    request = {
        "request_id": request_id,
        "method": "GetUpdateReadiness",
        "auth": {},
        "params": {
            "target_kind": "CORE_RUNTIME",
            "requires_restart": True,
            "expected_catalog_generation": generation,
            "expected_activity_sources": sources,
        },
    }
    executable = root / broker_binary.as_posix().lstrip("/")
    socket_path = root / broker_socket.as_posix().lstrip("/")
    _reject_symlink_components(executable)
    if _digest(executable) != broker_binary_sha256 or not os.access(executable, os.X_OK):
        raise ManagedRuntimeError("active broker binary no longer matches the verified release identity")
    try:
        response = subprocess.run(
            [str(executable), "request", "--socket", str(socket_path), "--operator"],
            input=json.dumps(request, separators=(",", ":")) + "\n",
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ManagedRuntimeError(f"authoritative broker readiness is unavailable: {error}") from error
    if response.returncode != 0:
        raise ManagedRuntimeError("authoritative broker readiness request was rejected")
    lines = response.stdout.splitlines()
    if len(lines) != 1:
        raise ManagedRuntimeError("broker returned an invalid JSONL response")
    try:
        envelope = json.loads(lines[0])
    except json.JSONDecodeError as error:
        raise ManagedRuntimeError("broker returned invalid JSON") from error
    if not isinstance(envelope, dict) or envelope.get("request_id") != request_id:
        raise ManagedRuntimeError("broker response request ID does not match the readiness query")
    result = envelope.get("result") if isinstance(envelope, dict) else None
    if not isinstance(result, dict):
        error = envelope.get("error") if isinstance(envelope, dict) else None
        code = error.get("code") if isinstance(error, dict) else "UNKNOWN"
        raise ManagedRuntimeError(f"broker did not return readiness: {code}")
    return _validate_readiness(result, generation)


def _ensure_shared_directory(path: Path, gid: int) -> None:
    """Create a dedicated shared directory or verify access without broad repair."""

    _reject_symlink_components(path)
    created = False
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        path.mkdir(parents=True, mode=0o2770)
        metadata = path.lstat()
        created = True
    if not stat.S_ISDIR(metadata.st_mode):
        raise ManagedRuntimeError(f"runtime data path is not a real directory: {path}")
    if created:
        os.chown(path, 0, gid)
        os.chmod(path, 0o2770)
        return
    metadata = path.stat()
    if metadata.st_gid != gid or stat.S_IMODE(metadata.st_mode) & 0o070 != 0o070:
        raise ManagedRuntimeError(f"existing runtime data directory lacks cyrene group access: {path}")


def _ensure_product_directory(path: Path, cyrene_uid: int, cyrene_gid: int) -> None:
    """Create or verify the narrow config directory needed by Product users."""

    _reject_symlink_components(path)
    created = False
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        path.mkdir(parents=True, mode=0o750)
        metadata = path.lstat()
        created = True
    if not stat.S_ISDIR(metadata.st_mode):
        raise ManagedRuntimeError("runtime configuration path is not a real directory")
    if created:
        os.chown(path, 0, cyrene_gid)
        os.chmod(path, 0o750)
        return
    metadata = path.stat()
    mode = stat.S_IMODE(metadata.st_mode)
    product_can_traverse = (
        (metadata.st_uid == cyrene_uid and mode & 0o100)
        or (metadata.st_gid == cyrene_gid and mode & 0o010)
        or bool(mode & 0o001)
    )
    if not product_can_traverse:
        raise ManagedRuntimeError("existing runtime configuration directory is not traversable by Product cyrene")


def _ensure_signing_key(path: Path, cyrene_uid: int, cyrene_gid: int) -> None:
    """Create a missing Product-owned key or reject any existing key drift."""

    _reject_symlink_components(path)
    existing = _safe_existing_file(path)
    if existing is None:
        _ensure_product_directory(path.parent, cyrene_uid, cyrene_gid)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(secrets.token_bytes(32))
            stream.flush()
            os.fsync(stream.fileno())
        os.chown(path, cyrene_uid, 0)
        os.chmod(path, 0o600)
        return
    metadata = path.stat()
    if len(existing) != 32 or metadata.st_uid != cyrene_uid or stat.S_IMODE(metadata.st_mode) != 0o600:
        raise ManagedRuntimeError("existing signing key must be a 32-byte cyrene-owned mode-0600 file")


def _atomic_manifest(path: Path, value: dict[str, Any], cyrene_gid: int) -> None:
    """Atomically publish the exact Product contract as root:cyrene 0640."""

    _reject_symlink_components(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o750)
    if path.parent.is_symlink():
        raise ManagedRuntimeError("runtime manifest directory is a symlink")
    existing = _safe_existing_file(path)
    if existing is not None:
        metadata = path.stat()
        if metadata.st_uid != 0 or metadata.st_gid != cyrene_gid or stat.S_IMODE(metadata.st_mode) != 0o640:
            raise ManagedRuntimeError("existing runtime manifest ownership or mode is mismatched")
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".pending", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(value, stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chown(temporary, 0, cyrene_gid)
        os.chmod(temporary, 0o640)
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def observe(identity_path: Path, root: Path = Path("/")) -> dict[str, Any]:
    """Verify signed runtime identity and authoritative live state, then project.

    The installer supplies ``identity_path`` only after verifying its signed
    native component assets. This tool independently checks those exact unit
    and binary bytes against the running processes before observing the broker.
    """

    if os.geteuid() != 0:
        raise ManagedRuntimeError("observe requires root")
    identities = _load_trusted_identity(identity_path)
    try:
        kernel_uid, cyrene_gid, maintenance_gid = _account_ids()
        cyrene_uid = pwd.getpwnam("cyrene").pw_uid
    except KeyError as error:
        raise ManagedRuntimeError(f"required managed account is missing: {error}") from error
    if kernel_uid < 0:
        raise ManagedRuntimeError("cyrene-kernel account is missing")
    _verify_managed_dropins(root, kernel_uid, cyrene_gid)
    active: dict[str, tuple[int, int, int, set[int]]] = {}
    for unit in UNITS.values():
        active[unit] = _verify_active_unit(unit, identities["units"][unit], root, kernel_uid, cyrene_gid)
        expected_uid = kernel_uid if unit == UNITS["kernel"] else 0
        if active[unit][1:3] != (expected_uid, cyrene_gid):
            raise ManagedRuntimeError(f"active process credentials do not match the formal unit: {unit}")
    kernel_unit = UNITS["kernel"]
    kernel_identity = active[kernel_unit]
    kernel_socket = root / DEFAULT_PATHS["kernelSocket"].lstrip("/")
    _verify_kernel_peer(kernel_socket, kernel_identity[:3])
    readiness = _broker_readiness(
        root,
        root / DEFAULT_PATHS["catalog"].lstrip("/"),
        Path(DEFAULT_PATHS["brokerSocket"]),
        Path(identities["units"][UNITS["runtimeMaintenance"]]["binaryPath"]),
        identities["units"][UNITS["runtimeMaintenance"]]["binarySha256"],
    )
    _validate_readiness(readiness)

    artifact_root = root / DEFAULT_PATHS["artifactRoot"].lstrip("/")
    installations_root = root / DEFAULT_PATHS["installationsRoot"].lstrip("/")
    signing_key = root / DEFAULT_PATHS["key"].lstrip("/")
    manifest = root / DEFAULT_PATHS["manifest"].lstrip("/")
    _ensure_shared_directory(artifact_root, cyrene_gid)
    _ensure_shared_directory(installations_root, cyrene_gid)
    _ensure_product_directory(manifest.parent, cyrene_uid, cyrene_gid)
    _ensure_signing_key(signing_key, cyrene_uid, cyrene_gid)
    projection = {
        "schemaVersion": 1,
        "profile": PROFILE,
        "status": "READY",
        "artifactRoot": str(Path(DEFAULT_PATHS["artifactRoot"])),
        "installationsRoot": str(Path(DEFAULT_PATHS["installationsRoot"])),
        "signingKeyFile": str(Path(DEFAULT_PATHS["key"])),
        "kernel": {"socket": str(Path(DEFAULT_PATHS["kernelSocket"]))},
        "components": {key: {"status": "READY"} for key in COMPONENT_UNITS},
    }
    _atomic_manifest(manifest, projection, cyrene_gid)
    return {
        "status": "READY",
        "manifest": str(manifest),
        "authorityStatus": readiness["status"],
        "activeWorkers": readiness["active_worker_count"],
        "activeAllocations": readiness["active_allocation_count"],
        "kernelUid": kernel_uid,
    }


def main(argv: list[str] | None = None) -> int:
    """Run one bounded prepare or observe action."""

    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="stage missing account and systemd drop-ins only")
    prepare_parser.add_argument("--root", type=Path, default=Path("/"), help=argparse.SUPPRESS)
    observe_parser = commands.add_parser("observe", help="verify live runtime and write Product projection")
    observe_parser.add_argument(
        "--identity-json",
        type=Path,
        required=True,
        help=(
            "wrapper-created JSON with schemaVersion=1 and units mapping each formal unit to "
            "unitFile/unitSha256 and active binaryPath/binarySha256 from verified signed releases"
        ),
    )
    observe_parser.add_argument("--root", type=Path, default=Path("/"), help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    try:
        result = prepare(args.root) if args.command == "prepare" else observe(args.identity_json, args.root)
    except (ManagedRuntimeError, OSError, subprocess.SubprocessError, KeyError, ValueError) as error:
        print(f"cyrene-managed-runtime: BLOCKED: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
