"""
┌─────────────────────────────────────────────────────────────────────┐
│  Module: tooling.runtime.cyrene_runtime                             │
│  Role: Bootstrap the canonical local GPU Kernel process topology.   │
│                                                                     │
│  模块职责：构建并拉起本地 GPU Kernel 拓扑，输出稳定的私有运行时配置。       │
└─────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PROFILE = "CYRENE_TEXT_LIFECYCLE_V1_LOCAL_GPU"
NATIVE_PROFILE = "NATIVE_LINUX_PROFILE"
WSL_DEV_PROFILE = "WSL_DEV_PROFILE"
SCHEMA_VERSION = 1
COMPONENT_ORDER = ("nvidiaAdapter", "sandboxd", "kernel")
EXECUTABLES = {
    "nvidiaAdapter": "cyrene-nvidia-adapter",
    "sandboxd": "cyrene-sandboxd",
    "kernel": "cyrene-kernel",
}
PACKAGES = tuple(EXECUTABLES.values())


class BootstrapFailure(RuntimeError):
    """Fail-closed bootstrap error with a stable, path-free public code."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code


@dataclass(frozen=True)
class Layout:
    """Private paths rooted under one explicit runtime home."""

    home: Path
    bin: Path
    build: Path
    run: Path
    state: Path
    logs: Path
    artifacts: Path
    installations: Path
    workers: Path
    signing_key: Path
    manifest: Path
    processes: Path
    lock: Path

    @classmethod
    def create(cls, home: Path) -> Layout:
        """Create the bounded runtime layout without deleting prior evidence."""

        if not home.expanduser().is_absolute():
            raise BootstrapFailure("RUNTIME_HOME_INVALID", "CYRENE_RUNTIME_HOME must be a bounded absolute path")
        resolved = home.expanduser().resolve()
        if resolved == Path(resolved.anchor):
            raise BootstrapFailure("RUNTIME_HOME_INVALID", "CYRENE_RUNTIME_HOME must be a bounded absolute path")
        layout = cls(
            home=resolved,
            bin=resolved / "bin",
            build=resolved / "build" / "platform",
            run=resolved / "run",
            state=resolved / "state",
            logs=resolved / "logs",
            artifacts=resolved / "artifacts",
            installations=resolved / "installations",
            workers=resolved / "run" / "workers",
            signing_key=resolved / "private" / "installer.key",
            manifest=resolved / "runtime.json",
            processes=resolved / "state" / "processes.json",
            lock=resolved / "state" / "bootstrap.lock",
        )
        for directory in (
            layout.bin,
            layout.build,
            layout.run,
            layout.state,
            layout.logs,
            layout.artifacts,
            layout.installations,
            layout.workers,
            layout.signing_key.parent,
        ):
            directory.mkdir(parents=True, exist_ok=True, mode=0o700)
            directory.chmod(0o700)
        return layout


def _atomic_json(path: Path, value: Any, mode: int = 0o600) -> None:
    """Persist one JSON document atomically with private permissions."""

    pending = path.with_suffix(path.suffix + ".pending")
    fd = os.open(pending, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(pending, path)
    path.chmod(mode)


def _platform_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _platform_revision(root: Path) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    revision = result.stdout.strip()
    if len(revision) != 40:
        raise BootstrapFailure("PLATFORM_REVISION_INVALID", "Platform revision is unavailable")
    return revision


def _is_wsl() -> bool:
    try:
        release = Path("/proc/sys/kernel/osrelease").read_text(encoding="utf-8")
    except OSError:
        return False
    return "microsoft" in release.lower() or "wsl" in release.lower()


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def _prepare_signing_key(layout: Layout) -> None:
    if layout.signing_key.exists():
        if layout.signing_key.stat().st_mode & 0o077 or layout.signing_key.stat().st_size != 32:
            raise BootstrapFailure("INSTALLER_KEY_INVALID", "Existing installer key must be 32 bytes with mode 0600")
        return
    fd = os.open(layout.signing_key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(os.urandom(32))
        stream.flush()
        os.fsync(stream.fileno())


def _build_binaries(root: Path, layout: Layout) -> dict[str, Path]:
    """Build exact-source binaries and install stable copies under runtime home."""

    cargo = shutil.which("cargo")
    if cargo is None:
        raise BootstrapFailure("CARGO_UNAVAILABLE", "Install the pinned Rust toolchain")
    build_log = layout.logs / "platform-build.log"
    with build_log.open("ab") as output:
        command = [cargo, "build", "--locked", "--release", "--target-dir", str(layout.build)]
        for package in PACKAGES:
            command.extend(("-p", package))
        result = subprocess.run(
            command,
            cwd=root,
            stdout=output,
            stderr=subprocess.STDOUT,
            timeout=1800,
        )
    if result.returncode:
        raise BootstrapFailure("PLATFORM_BUILD_FAILED", "Platform runtime binaries did not build from the lockfile")
    installed: dict[str, Path] = {}
    for component, executable in EXECUTABLES.items():
        source = layout.build / "release" / executable
        if not source.is_file():
            raise BootstrapFailure("RUNTIME_BINARY_MISSING", f"Build omitted {executable}")
        target = layout.bin / executable
        pending = target.with_suffix(".pending")
        shutil.copyfile(source, pending)
        pending.chmod(0o700)
        os.replace(pending, target)
        installed[component] = target
    return installed


def _installed_binaries(layout: Layout) -> dict[str, Path]:
    installed = {component: layout.bin / name for component, name in EXECUTABLES.items()}
    if any(not path.is_file() or not os.access(path, os.X_OK) for path in installed.values()):
        raise BootstrapFailure("RUNTIME_BINARY_MISSING", "Run bootstrap without --no-build to install binaries")
    return installed


def _nvidia_probe() -> dict[str, Any]:
    executable = shutil.which("nvidia-smi")
    if executable is None:
        raise BootstrapFailure("NVIDIA_SMI_UNAVAILABLE", "nvidia-smi is required")
    result = subprocess.run(
        [
            executable,
            "--query-gpu=index,name,memory.total",
            "--format=csv,noheader,nounits",
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    rows = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if result.returncode or not rows:
        raise BootstrapFailure("NVIDIA_GPU_UNAVAILABLE", "NVIDIA inventory probe failed")
    fields = [part.strip() for part in rows[0].split(",")]
    if len(fields) != 3 or not fields[2].isdigit():
        raise BootstrapFailure("NVIDIA_INVENTORY_INVALID", "NVIDIA inventory is incomplete")
    return {"count": len(rows), "name": fields[1], "memoryMiB": int(fields[2])}


def _proc_start_time(pid: int) -> int | None:
    try:
        suffix = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").rsplit(")", 1)[1]
        return int(suffix.split()[19])
    except (OSError, IndexError, ValueError):
        return None


def _alive(record: dict[str, Any]) -> bool:
    pid = record.get("pid")
    start_time = record.get("startTime")
    return type(pid) is int and type(start_time) is int and _proc_start_time(pid) == start_time


def _load_processes(layout: Layout) -> dict[str, dict[str, Any]]:
    if not layout.processes.exists():
        return {}
    try:
        value = json.loads(layout.processes.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime process state is unreadable") from exc
    if not isinstance(value, dict):
        raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime process state is invalid")
    return {str(name): record for name, record in value.items() if isinstance(record, dict)}


def _wait_for_socket(process: subprocess.Popen[bytes], path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise BootstrapFailure("RUNTIME_COMPONENT_EXITED", "A runtime component exited during health check")
        try:
            if path.is_socket():
                return
        except OSError:
            pass
        time.sleep(0.1)
    raise BootstrapFailure("RUNTIME_HEALTH_TIMEOUT", "A runtime component did not become ready")


def _start_component(
    name: str, command: list[str], socket_path: Path, layout: Layout
) -> tuple[subprocess.Popen[bytes], dict[str, Any]]:
    """Start one owned process and require its declared UDS readiness."""

    log = (layout.logs / f"{name}.log").open("ab", buffering=0)
    process = subprocess.Popen(
        command,
        cwd=_platform_root(),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    log.close()
    try:
        _wait_for_socket(process, socket_path, 20)
    except Exception:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
        raise
    start_time = _proc_start_time(process.pid)
    if start_time is None:
        process.terminate()
        raise BootstrapFailure("RUNTIME_PROCESS_IDENTITY_LOST", "Process identity is unavailable")
    return process, {"pid": process.pid, "startTime": start_time, "socket": str(socket_path)}


def _remove_owned_sockets(layout: Layout) -> None:
    for name in ("nvidia.sock", "sandboxd.sock", "kernel.sock", "worker.sock", "provider.sock"):
        path = layout.run / name
        try:
            if path.is_socket():
                path.unlink()
        except OSError:
            continue


def _stop_records(records: dict[str, dict[str, Any]], timeout: float = 15) -> bool:
    """Stop exact recorded process groups in reverse dependency order."""

    forced = False
    selected = [records[name] for name in reversed(COMPONENT_ORDER) if name in records]
    for record in selected:
        if _alive(record):
            try:
                os.killpg(record["pid"], signal.SIGTERM)
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and any(_alive(record) for record in selected):
        time.sleep(0.1)
    for record in selected:
        if _alive(record):
            forced = True
            try:
                os.killpg(record["pid"], signal.SIGKILL)
            except ProcessLookupError:
                pass
    return forced


def _host_projection(wsl: bool) -> dict[str, Any]:
    return {
        "host": "Windows" if wsl else "Linux",
        "gpuRuntime": "WSL2_CUDA" if wsl else "NATIVE_LINUX_CUDA",
        "hardIsolation": not wsl,
    }


def _public_evidence(manifest: dict[str, Any]) -> dict[str, Any]:
    """Return only fields approved for CI/public acceptance evidence."""

    components = manifest.get("components", {})
    return {
        "schemaVersion": SCHEMA_VERSION,
        "profile": manifest.get("profile"),
        "runtimeMode": manifest.get("runtimeMode"),
        "status": manifest.get("status"),
        "platformRevision": manifest.get("platformRevision"),
        "host": manifest.get("host"),
        "gpu": manifest.get("gpu"),
        "components": {
            name: value.get("status", "UNAVAILABLE") for name, value in components.items() if isinstance(value, dict)
        },
    }


def _manifest(
    *,
    layout: Layout,
    revision: str,
    profile: str,
    wsl: bool,
    gpu: dict[str, Any],
    processes: dict[str, dict[str, Any]],
    binaries: dict[str, Path],
) -> dict[str, Any]:
    return {
        "schemaVersion": SCHEMA_VERSION,
        "profile": PROFILE,
        "runtimeMode": profile,
        "status": "READY",
        "platformRevision": revision,
        "host": _host_projection(wsl),
        "gpu": gpu,
        "runtimeHome": str(layout.home),
        "artifactRoot": str(layout.artifacts),
        "installationsRoot": str(layout.installations),
        "signingKeyFile": str(layout.signing_key),
        "kernel": {
            "socket": str(layout.run / "kernel.sock"),
            "workerControlSocket": str(layout.run / "worker.sock"),
            "providerSocket": str(layout.run / "provider.sock"),
        },
        "components": {
            name: {
                **processes[name],
                "status": "READY",
                "binary": str(binaries[name]),
                "binaryDigest": _sha256(binaries[name]),
            }
            for name in COMPONENT_ORDER
        },
    }


def up(args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Build and start the canonical adapters and Kernel in dependency order."""

    if sys.platform != "linux":
        raise BootstrapFailure("RUNTIME_OS_UNSUPPORTED", "The V1 runtime requires Linux")
    wsl = _is_wsl()
    if wsl and args.profile != WSL_DEV_PROFILE:
        raise BootstrapFailure("WSL_PROFILE_REQUIRED", "WSL shared-device mode requires explicit WSL_DEV_PROFILE")
    if not wsl and args.profile == WSL_DEV_PROFILE:
        raise BootstrapFailure("WSL_PROFILE_INVALID", "WSL_DEV_PROFILE is accepted only on WSL2")
    existing = _load_processes(layout)
    if any(_alive(record) for record in existing.values()):
        raise BootstrapFailure("RUNTIME_ALREADY_RUNNING", "Use runtime status or down first")

    root = _platform_root()
    revision = _platform_revision(root)
    gpu = _nvidia_probe()
    _prepare_signing_key(layout)
    binaries = _installed_binaries(layout) if args.no_build else _build_binaries(root, layout)
    uid = os.getuid()
    sockets = {
        "nvidiaAdapter": layout.run / "nvidia.sock",
        "sandboxd": layout.run / "sandboxd.sock",
        "kernel": layout.run / "kernel.sock",
    }
    _remove_owned_sockets(layout)
    commands = {
        "nvidiaAdapter": [
            str(binaries["nvidiaAdapter"]),
            "--socket",
            str(sockets["nvidiaAdapter"]),
            "--allowed-client-uid",
            str(uid),
            *(["--wsl-shared-device"] if wsl else []),
        ],
        "sandboxd": [
            str(binaries["sandboxd"]),
            "--socket",
            str(sockets["sandboxd"]),
            "--transport-root",
            str(layout.workers),
            "--allowed-client-uid",
            str(uid),
            *(["--dev-mode"] if wsl else []),
            *(["--cgroup-root", str(args.sandbox_cgroup_root)] if args.sandbox_cgroup_root else []),
        ],
        "kernel": [
            str(binaries["kernel"]),
            "--node-id",
            args.node_id,
            "--socket",
            str(sockets["kernel"]),
            "--worker-control-socket",
            str(layout.run / "worker.sock"),
            "--provider-socket",
            str(layout.run / "provider.sock"),
            "--hardware-adapter",
            "nvidia=" + str(sockets["nvidiaAdapter"]),
            "--hardware-adapter-peer-uid",
            "nvidia=" + str(uid),
            "--sandbox-adapter",
            "sandboxd=" + str(sockets["sandboxd"]),
            "--sandbox-adapter-peer-uid",
            str(uid),
            "--installations-root",
            str(layout.installations),
            "--worker-transport-root",
            str(layout.workers),
            "--runtime-journal",
            str(layout.state / "kernel-journal.jsonl"),
        ],
    }
    processes: dict[str, dict[str, Any]] = {}
    try:
        for name in COMPONENT_ORDER:
            _process, record = _start_component(name, commands[name], sockets[name], layout)
            processes[name] = record
            _atomic_json(layout.processes, processes)
        _nvidia_probe()
        manifest = _manifest(
            layout=layout,
            revision=revision,
            profile=args.profile,
            wsl=wsl,
            gpu=gpu,
            processes=processes,
            binaries=binaries,
        )
        _atomic_json(layout.manifest, manifest)
        return _public_evidence(manifest)
    except Exception:
        _stop_records(processes)
        _remove_owned_sockets(layout)
        _atomic_json(layout.processes, {})
        raise


def status(_args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Read exact process identities and report sanitized component health."""

    processes = _load_processes(layout)
    states = {name: "READY" if _alive(processes.get(name, {})) else "UNAVAILABLE" for name in COMPONENT_ORDER}
    manifest: dict[str, Any]
    try:
        manifest = json.loads(layout.manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        manifest = {
            "profile": PROFILE,
            "runtimeMode": None,
            "platformRevision": None,
            "host": _host_projection(_is_wsl()),
            "gpu": None,
        }
    manifest["status"] = "READY" if all(value == "READY" for value in states.values()) else "DOWN"
    manifest["components"] = {name: {"status": value} for name, value in states.items()}
    return _public_evidence(manifest)


def down(_args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Gracefully stop owned processes and clear only owned UDS files."""

    processes = _load_processes(layout)
    forced = _stop_records(processes)
    _remove_owned_sockets(layout)
    _atomic_json(layout.processes, {})
    if layout.manifest.exists():
        try:
            manifest = json.loads(layout.manifest.read_text(encoding="utf-8"))
            manifest["status"] = "DOWN"
            for value in manifest.get("components", {}).values():
                if isinstance(value, dict):
                    value["status"] = "DOWN"
            _atomic_json(layout.manifest, manifest)
        except (OSError, json.JSONDecodeError):
            pass
    return {"schemaVersion": SCHEMA_VERSION, "profile": PROFILE, "status": "DOWN", "forced": forced}


def parser() -> argparse.ArgumentParser:
    """Define the stable bootstrap command surface."""

    value = argparse.ArgumentParser(description="Cyrene canonical local GPU runtime")
    value.add_argument("command", choices=("up", "down", "status"))
    value.add_argument(
        "--runtime-home",
        type=Path,
        default=Path(os.environ["CYRENE_RUNTIME_HOME"]) if os.environ.get("CYRENE_RUNTIME_HOME") else None,
    )
    value.add_argument("--profile", choices=(NATIVE_PROFILE, WSL_DEV_PROFILE), default=NATIVE_PROFILE)
    value.add_argument("--node-id", default=os.environ.get("CYRENE_NODE_ID", "cyrene-reference-node"))
    value.add_argument("--sandbox-cgroup-root", type=Path)
    value.add_argument("--no-build", action="store_true")
    return value


def main() -> int:
    """Execute one locked runtime operation and print sanitized JSON only."""

    args = parser().parse_args()
    if args.runtime_home is None:
        print(json.dumps({"status": "FAILED", "code": "RUNTIME_HOME_REQUIRED"}))
        return 2
    try:
        layout = Layout.create(args.runtime_home)
        with layout.lock.open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = {"up": up, "down": down, "status": status}[args.command](args, layout)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (BlockingIOError, BootstrapFailure, OSError, subprocess.SubprocessError) as exc:
        code = exc.code if isinstance(exc, BootstrapFailure) else "RUNTIME_BOOTSTRAP_FAILED"
        print(json.dumps({"status": "FAILED", "code": code}, sort_keys=True))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
