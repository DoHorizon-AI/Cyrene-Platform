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
import errno
import fcntl
import hashlib
import json
import os
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PROFILE = "CYRENE_PLATFORM_RUNTIME_V1_LOCAL_GPU"
NATIVE_PROFILE = "NATIVE_LINUX_PROFILE"
WSL_DEV_PROFILE = "WSL_DEV_PROFILE"
SCHEMA_VERSION = 1
COMPONENT_ORDER = ("systemAdapter", "nvidiaAdapter", "sandboxd", "kernel")
EXECUTABLES = {
    "systemAdapter": "cyrene-linux-sys-adapter",
    "nvidiaAdapter": "cyrene-nvidia-adapter",
    "sandboxd": "cyrene-sandboxd",
    "kernel": "cyrene-kernel",
}
PACKAGES = tuple(EXECUTABLES.values())
COMPONENT_READY_TIMEOUT_SECONDS = 20.0
COMPONENT_STOP_GRACE_SECONDS = 3.0
FORCED_STOP_WAIT_SECONDS = 1.0
SOCKET_POLL_INTERVAL_SECONDS = 0.1
SOCKET_CONNECT_TIMEOUT_SECONDS = 0.5
MAX_READINESS_FRAME_BYTES = 1024 * 1024
HTTP2_PREFACE = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"


class BootstrapFailure(RuntimeError):
    """Fail-closed bootstrap error with a stable, path-free public code.

    中文:以稳定且不含路径的公开错误码表示启动失败,并按 fail-closed 方式处理。
    """

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.process_record: tuple[str, dict[str, Any]] | None = None
        self.process_handle: subprocess.Popen[bytes] | None = None


@dataclass(frozen=True)
class Layout:
    """Private paths rooted under one explicit runtime home.

    中文:所有私有路径均位于一个明确指定的 runtime home 下。
    """

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
        """Create the bounded runtime layout without deleting prior evidence.

        中文:创建边界明确的 runtime 目录结构,不删除已有证据。
        """

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
    """Persist one JSON document atomically with private permissions.

    中文:以原子方式并使用私有权限持久化单个 JSON 文档。
    """

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
    """Build developer binaries from this checkout and install them privately.

    中文:仅在显式开发构建时从当前源码构建二进制,并安装到 runtime home。
    """

    cargo = shutil.which("cargo")
    if cargo is None:
        raise BootstrapFailure("CARGO_UNAVAILABLE", "Install the pinned Rust toolchain")
    start_time = time.monotonic()
    print(
        "Developer source build started (cargo build --locked --release); duration is measured, not estimated.",
        file=sys.stderr,
    )
    build_log = layout.logs / "platform-build.log"
    command = [cargo, "build", "--locked", "--release", "--target-dir", str(layout.build)]
    for package in PACKAGES:
        command.extend(("-p", package))
    try:
        with build_log.open("ab") as output:
            result = subprocess.run(
                command,
                cwd=root,
                stdout=output,
                stderr=subprocess.STDOUT,
                timeout=1800,
            )
    except subprocess.TimeoutExpired as exc:
        elapsed = time.monotonic() - start_time
        print(f"Developer source build timed out after {elapsed:.2f}s", file=sys.stderr)
        raise BootstrapFailure(
            "PLATFORM_BUILD_TIMEOUT",
            f"Developer source build exceeded its 1800-second bound (elapsed {elapsed:.2f}s)",
        ) from exc
    elapsed = time.monotonic() - start_time
    if result.returncode:
        print(f"Developer source build failed after {elapsed:.2f}s", file=sys.stderr)
        raise BootstrapFailure(
            "PLATFORM_BUILD_FAILED",
            f"Platform runtime binaries did not build from the lockfile (failed after {elapsed:.2f}s)",
        )
    print(f"Developer source build completed in {elapsed:.2f}s", file=sys.stderr)
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
        raise BootstrapFailure(
            "RUNTIME_BINARY_MISSING",
            "Preinstalled runtime binaries are missing; install them with the official installer or use --build-from-source for development",
        )
    return installed


def _nvidia_probe() -> dict[str, Any] | None:
    """Capture optional host GPU evidence without gating node readiness.

    中文:尽力采集可选 GPU 事实,但不把它作为节点启动就绪门槛。
    """

    executable = shutil.which("nvidia-smi")
    if executable is None:
        return None
    try:
        result = subprocess.run(
            [
                executable,
                "--query-gpu=index,name,memory.total",
                "--format=csv,noheader,nounits",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    rows = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if result.returncode or not rows:
        return None
    fields = [part.strip() for part in rows[0].split(",")]
    if len(fields) != 3 or not fields[2].isdigit():
        return None
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
    if not isinstance(value, dict) or any(not isinstance(record, dict) for record in value.values()):
        raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime process state is invalid")
    return value


def _component_socket_paths(layout: Layout) -> dict[str, tuple[Path, ...]]:
    """Return the complete UDS set owned by each launched component.

    中文:返回每个受管理组件负责创建的完整 UDS 路径集合。
    """

    return {
        "systemAdapter": (layout.run / "system.sock",),
        "nvidiaAdapter": (layout.run / "nvidia.sock",),
        "sandboxd": (layout.run / "sandboxd.sock",),
        "kernel": (layout.run / "kernel.sock", layout.run / "worker.sock", layout.run / "provider.sock"),
    }


def _socket_identity(path: Path) -> dict[str, int] | None:
    """Read one socket's inode identity, returning None only when absent.

    中文:读取 socket 的 inode 身份;仅在路径不存在时返回 None。
    """

    try:
        info = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise BootstrapFailure("RUNTIME_SOCKET_ACCESS_FAILED", "A runtime socket could not be inspected") from exc
    if not stat.S_ISSOCK(info.st_mode):
        raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint is not an owned socket")
    return {"device": info.st_dev, "inode": info.st_ino}


def _is_socket_identity(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"device", "inode"}
        and type(value["device"]) is int
        and type(value["inode"]) is int
    )


def _connect_socket_peer_pid(path: Path, timeout: float) -> int | None:
    """Connect to a local UDS and return the listener PID from SO_PEERCRED.

    中文:连接本地 UDS,并从 SO_PEERCRED 读取监听进程 PID。
    """

    peer_credentials = getattr(socket, "SO_PEERCRED", None)
    if peer_credentials is None:
        raise BootstrapFailure("RUNTIME_SOCKET_PEER_ID_UNAVAILABLE", "Linux UDS peer identity is unavailable")
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(max(0.01, timeout))
            stream.connect(str(path))
            credentials = stream.getsockopt(socket.SOL_SOCKET, peer_credentials, struct.calcsize("3i"))
            peer_pid, _peer_uid, _peer_gid = struct.unpack("3i", credentials)
            return peer_pid
    except (FileNotFoundError, ConnectionRefusedError, ConnectionResetError, TimeoutError, BlockingIOError):
        return None
    except (OSError, struct.error) as exc:
        if isinstance(exc, OSError) and exc.errno in {errno.ECONNREFUSED, errno.ENOENT, errno.ETIMEDOUT}:
            return None
        raise BootstrapFailure("RUNTIME_SOCKET_CONNECT_FAILED", "A runtime socket readiness probe failed") from exc


def _receive_exact(stream: socket.socket, size: int, deadline: float) -> bytes:
    """Read an exact bounded protocol fragment before one shared deadline.

    中文:在共享 deadline 前读取定长协议片段。
    """

    chunks: list[bytes] = []
    received = 0
    while received < size:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("readiness protocol probe timed out")
        stream.settimeout(remaining)
        chunk = stream.recv(size - received)
        if not chunk:
            raise ConnectionError("readiness protocol peer closed early")
        chunks.append(chunk)
        received += len(chunk)
    return b"".join(chunks)


def _probe_component_socket(name: str, path: Path, timeout: float) -> int | None:
    """Connect and perform a harmless protocol probe, returning the peer PID.

    中文:建立连接并执行无副作用协议探测,返回对端 PID。
    """

    peer_credentials = getattr(socket, "SO_PEERCRED", None)
    if peer_credentials is None:
        raise BootstrapFailure("RUNTIME_SOCKET_PEER_ID_UNAVAILABLE", "Linux UDS peer identity is unavailable")
    deadline = time.monotonic() + max(0.01, timeout)
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(max(0.01, timeout))
            stream.connect(str(path))
            credentials = stream.getsockopt(socket.SOL_SOCKET, peer_credentials, struct.calcsize("3i"))
            peer_pid, _peer_uid, _peer_gid = struct.unpack("3i", credentials)
            if name in {"systemAdapter", "nvidiaAdapter", "sandboxd"}:
                # An empty, valid protocol frame asks each server to reject an
                # unsupported request version without probing host resources.
                stream.sendall(b"\x00\x00\x00\x00")
                length = int.from_bytes(_receive_exact(stream, 4, deadline), "big")
                if length == 0 or length > MAX_READINESS_FRAME_BYTES:
                    return None
                _receive_exact(stream, length, deadline)
            elif name == "kernel":
                # Complete the HTTP/2 connection preface so tonic can accept the
                # probe without logging a malformed gRPC connection.
                stream.sendall(HTTP2_PREFACE + b"\x00\x00\x00\x04\x00\x00\x00\x00\x00")
                header = _receive_exact(stream, 9, deadline)
                length = int.from_bytes(header[:3], "big")
                frame_type = header[3]
                stream_id = int.from_bytes(header[5:9], "big") & 0x7FFFFFFF
                if length > MAX_READINESS_FRAME_BYTES or frame_type != 4 or stream_id != 0:
                    return None
                _receive_exact(stream, length, deadline)
                stream.sendall(b"\x00\x00\x00\x04\x01\x00\x00\x00\x00")
            else:
                raise BootstrapFailure("RUNTIME_SOCKET_LAYOUT_INVALID", "Unknown runtime component socket")
            return peer_pid
    except BootstrapFailure:
        raise
    except (OSError, TimeoutError, ConnectionError, struct.error):
        return None


def _wait_for_sockets(
    name: str,
    process: subprocess.Popen[bytes],
    paths: tuple[Path, ...],
    timeout: float,
    start_time: int,
    observed: dict[str, dict[str, int]],
) -> None:
    """Wait a bounded time for this process to listen on every owned endpoint.

    中文:在有界时间内等待组件在每个自有 endpoint 上监听,并核验 peer PID。
    """

    deadline = time.monotonic() + timeout
    connected: set[Path] = set()
    while True:
        if process.poll() is not None or _proc_start_time(process.pid) != start_time:
            raise BootstrapFailure("RUNTIME_COMPONENT_EXITED", "A runtime component exited during health check")
        for path in paths:
            current_identity = _socket_identity(path)
            if current_identity is None:
                continue
            identity_key = str(path)
            previous_identity = observed.get(identity_key)
            if previous_identity is not None and previous_identity != current_identity:
                raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint changed during startup")
            if path in connected:
                continue
            remaining = max(0.01, deadline - time.monotonic())
            peer_pid = _probe_component_socket(name, path, min(SOCKET_CONNECT_TIMEOUT_SECONDS, remaining))
            if peer_pid is None:
                continue
            if peer_pid != process.pid:
                raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint belongs to another process")
            if _socket_identity(path) != current_identity:
                raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint changed during startup")
            observed[identity_key] = current_identity
            connected.add(path)
        if len(connected) == len(paths):
            if process.poll() is not None or _proc_start_time(process.pid) != start_time:
                raise BootstrapFailure("RUNTIME_COMPONENT_EXITED", "A runtime component exited during health check")
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise BootstrapFailure("RUNTIME_HEALTH_TIMEOUT", "A runtime component did not become ready")
        time.sleep(min(SOCKET_POLL_INTERVAL_SECONDS, remaining))


def _wait_for_socket(process: subprocess.Popen[bytes], path: Path, timeout: float) -> None:
    """Compatibility wrapper for the single-socket readiness contract.

    中文:保留单 socket 调用接口,并应用相同的连接与 PID 校验。
    """

    process_start_time = _proc_start_time(process.pid)
    if process_start_time is None:
        raise BootstrapFailure("RUNTIME_PROCESS_IDENTITY_LOST", "Process identity is unavailable")
    deadline = time.monotonic() + max(0.0, timeout)
    while True:
        if process.poll() is not None or _proc_start_time(process.pid) != process_start_time:
            raise BootstrapFailure("RUNTIME_COMPONENT_EXITED", "A runtime component exited during health check")
        identity = _socket_identity(path)
        if identity is not None:
            remaining = max(0.01, deadline - time.monotonic())
            peer_pid = _connect_socket_peer_pid(path, min(SOCKET_CONNECT_TIMEOUT_SECONDS, remaining))
            if peer_pid is not None:
                if peer_pid != process.pid or _socket_identity(path) != identity:
                    raise BootstrapFailure(
                        "RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint belongs to another process"
                    )
                return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise BootstrapFailure("RUNTIME_HEALTH_TIMEOUT", "A runtime component did not become ready")
        time.sleep(min(SOCKET_POLL_INTERVAL_SECONDS, remaining))


def _signal_process_group(pid: int, received_signal: signal.Signals) -> None:
    """Signal one component's isolated process group, tolerating exit races.

    中文:向单个组件的隔离进程组发送信号,并容忍退出时的竞态。
    """

    try:
        os.killpg(pid, received_signal)
    except ProcessLookupError:
        return
    except OSError as exc:
        raise BootstrapFailure("RUNTIME_STOP_FAILED", "A runtime component could not be signalled") from exc


def _stop_process(process: subprocess.Popen[bytes], timeout: float = COMPONENT_STOP_GRACE_SECONDS) -> bool:
    """Stop one just-launched process group and reap its direct child.

    中文:停止刚启动的进程组并回收其直接子进程。
    """

    if process.poll() is not None:
        process.wait()
        return False
    forced = False
    _signal_process_group(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=max(0.0, timeout))
    except subprocess.TimeoutExpired:
        forced = True
        _signal_process_group(process.pid, signal.SIGKILL)
        try:
            process.wait(timeout=FORCED_STOP_WAIT_SECONDS)
        except subprocess.TimeoutExpired as exc:
            raise BootstrapFailure("RUNTIME_STOP_FAILED", "A runtime component did not stop after SIGKILL") from exc
    return forced


def _start_component(
    name: str, command: list[str], socket_paths: tuple[Path, ...], layout: Layout
) -> tuple[subprocess.Popen[bytes], dict[str, Any]]:
    """Start one owned process and require its declared UDS readiness.

    中文:启动一个受管理的进程,并要求其达到声明的 UDS 就绪状态。
    """

    if not socket_paths:
        raise BootstrapFailure("RUNTIME_SOCKET_LAYOUT_INVALID", "A runtime component has no declared socket")
    for path in socket_paths:
        if _socket_identity(path) is not None:
            raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint already exists")
    socket_path = socket_paths[0]
    log = (layout.logs / f"{name}.log").open("ab", buffering=0)
    try:
        process = subprocess.Popen(
            command,
            cwd=_platform_root(),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    finally:
        log.close()
    start_time = _proc_start_time(process.pid)
    observed_sockets: dict[str, dict[str, int]] = {}
    try:
        if start_time is None:
            raise BootstrapFailure("RUNTIME_PROCESS_IDENTITY_LOST", "Process identity is unavailable")
        _wait_for_sockets(
            name,
            process,
            socket_paths,
            COMPONENT_READY_TIMEOUT_SECONDS,
            start_time,
            observed_sockets,
        )
    except Exception as start_error:
        try:
            _stop_process(process)
            _remove_owned_sockets(
                layout,
                {
                    name: {
                        "socket": str(socket_path),
                        "ownedSockets": observed_sockets,
                    }
                },
                components=(name,),
            )
        except BootstrapFailure as cleanup_error:
            if start_time is not None:
                cleanup_error.process_record = (
                    name,
                    {
                        "pid": process.pid,
                        "startTime": start_time,
                        "socket": str(socket_path),
                        "ownedSockets": observed_sockets,
                    },
                )
            cleanup_error.process_handle = process
            raise cleanup_error from start_error
        raise
    return process, {
        "pid": process.pid,
        "startTime": start_time,
        "socket": str(socket_path),
        "ownedSockets": observed_sockets,
    }


def _remove_socket(path: Path, expected_identity: dict[str, int] | None) -> None:
    """Remove one socket only when its recorded inode identity still matches.

    中文:仅当 socket 的 inode 身份与所属记录一致时删除。
    """

    current_identity = _socket_identity(path)
    if current_identity is None:
        return
    if expected_identity is None or current_identity != expected_identity:
        raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint does not match its owner record")
    try:
        path.unlink()
    except FileNotFoundError:
        return
    except OSError as exc:
        raise BootstrapFailure("RUNTIME_SOCKET_CLEANUP_FAILED", "A runtime socket could not be removed") from exc


def _capture_owned_socket_identities(
    layout: Layout, records: dict[str, dict[str, Any]]
) -> dict[str, dict[str, Any]]:
    """Verify live socket peers and persist identities before stopping them.

    中文:停止活跃记录前核验 peer PID,并补存旧版记录缺少的 endpoint inode 身份。
    """

    paths_by_component = _component_socket_paths(layout)
    captured = {name: dict(record) for name, record in records.items()}
    for name, record in captured.items():
        expected_paths = paths_by_component.get(name)
        if expected_paths is None or record.get("socket") != str(expected_paths[0]):
            raise BootstrapFailure("RUNTIME_PROCESS_OWNERSHIP_UNKNOWN", "Runtime process ownership is ambiguous")
        owned = record.get("ownedSockets", {})
        if not isinstance(owned, dict):
            raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime socket ownership state is invalid")
        owned = dict(owned)
        permitted_paths = {str(path) for path in expected_paths}
        if any(path not in permitted_paths or not _is_socket_identity(identity) for path, identity in owned.items()):
            raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime socket ownership state is invalid")
        process_alive = _alive(record)
        for path in expected_paths:
            identity = _socket_identity(path)
            if identity is None:
                continue
            path_key = str(path)
            saved_identity = owned.get(path_key)
            pid = record.get("pid")
            if process_alive and (
                type(pid) is not int or _probe_component_socket(name, path, 0.5) != pid
            ):
                raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint has no verifiable owner")
            if saved_identity is not None:
                if saved_identity != identity:
                    raise BootstrapFailure(
                        "RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint does not match its owner record"
                    )
                continue
            if not process_alive:
                raise BootstrapFailure("RUNTIME_SOCKET_OWNERSHIP_UNKNOWN", "A runtime endpoint has no verifiable owner")
            owned[path_key] = identity
        record["ownedSockets"] = owned
    return captured


def _remove_owned_sockets(
    layout: Layout,
    records: dict[str, dict[str, Any]],
    *,
    components: tuple[str, ...] | None = None,
) -> None:
    """Remove runtime UDS files only when inode identities match owner records.

    中文:仅当 inode 身份与进程所属记录匹配时清理 runtime UDS。
    """

    all_paths_by_component = _component_socket_paths(layout)
    names = tuple(all_paths_by_component) if components is None else components
    paths_by_component = {name: all_paths_by_component[name] for name in names if name in all_paths_by_component}
    if len(paths_by_component) != len(names):
        raise BootstrapFailure("RUNTIME_SOCKET_LAYOUT_INVALID", "Unknown runtime component socket owner")
    expected_identities: dict[Path, dict[str, int]] = {}
    for name, record in records.items():
        paths = paths_by_component.get(name)
        if paths is None or record.get("socket") != str(paths[0]):
            raise BootstrapFailure("RUNTIME_PROCESS_OWNERSHIP_UNKNOWN", "Runtime process ownership is ambiguous")
        owned = record.get("ownedSockets", {})
        if not isinstance(owned, dict):
            raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime socket ownership state is invalid")
        permitted_paths = {str(path): path for path in paths}
        for path_key, identity in owned.items():
            path = permitted_paths.get(path_key) if isinstance(path_key, str) else None
            if path is None or not _is_socket_identity(identity):
                raise BootstrapFailure("RUNTIME_STATE_INVALID", "Runtime socket ownership state is invalid")
            expected_identities[path] = identity

    for paths in paths_by_component.values():
        for path in paths:
            current_identity = _socket_identity(path)
            if current_identity is None:
                continue
            _remove_socket(path, expected_identities.get(path))


def _stop_records(records: dict[str, dict[str, Any]], timeout: float = 15) -> bool:
    """Stop exact recorded process groups in reverse dependency order.

    中文:按依赖顺序的逆序停止已准确记录的进程组。
    """

    selected = [records[name] for name in reversed(COMPONENT_ORDER) if name in records and _alive(records[name])]
    if not selected:
        return False

    forced = False
    deadline = time.monotonic() + max(0.0, timeout)
    slot = max(0.0, timeout) / len(selected)
    for index, record in enumerate(selected):
        if not _alive(record):
            continue
        pid = record["pid"]
        _signal_process_group(pid, signal.SIGTERM)

        remaining_components = len(selected) - index
        remaining_time = max(0.0, deadline - time.monotonic())
        component_budget = min(slot, remaining_time / remaining_components)
        grace_deadline = min(deadline, time.monotonic() + min(COMPONENT_STOP_GRACE_SECONDS, component_budget * 0.8))
        while _alive(record) and time.monotonic() < grace_deadline:
            time.sleep(min(SOCKET_POLL_INTERVAL_SECONDS, grace_deadline - time.monotonic()))
        if _alive(record):
            forced = True
            _signal_process_group(pid, signal.SIGKILL)
            forced_deadline = min(deadline, time.monotonic() + min(FORCED_STOP_WAIT_SECONDS, component_budget * 0.2))
            while _alive(record) and time.monotonic() < forced_deadline:
                time.sleep(min(SOCKET_POLL_INTERVAL_SECONDS, forced_deadline - time.monotonic()))
            if _alive(record):
                raise BootstrapFailure("RUNTIME_STOP_FAILED", "A runtime component remained alive after SIGKILL")
    return forced


def _host_projection(wsl: bool, hard_isolation: bool | None = None) -> dict[str, Any]:
    """Project actual host isolation without overstating developer modes.

    中文:根据实际隔离路径投影 host 事实,避免开发模式夸大隔离能力。
    """

    if hard_isolation is None:
        hard_isolation = not wsl
    return {
        "host": "Windows" if wsl else "Linux",
        "gpuRuntime": "WSL2_CUDA" if wsl else "NATIVE_LINUX_CUDA",
        "hardIsolation": hard_isolation,
    }


def _public_evidence(manifest: dict[str, Any]) -> dict[str, Any]:
    """Return only fields approved for CI/public acceptance evidence.

    中文:仅返回获准用于 CI 或公开验收证据的字段。
    """

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
    gpu: dict[str, Any] | None,
    processes: dict[str, dict[str, Any]],
    binaries: dict[str, Path],
    dev_mode: bool,
    disable_device_bpf: bool,
) -> dict[str, Any]:
    """Build the private runtime manifest from observed startup state.

    中文:根据启动时观测到的状态构建私有 runtime manifest。
    """

    return {
        "schemaVersion": SCHEMA_VERSION,
        "profile": PROFILE,
        "runtimeMode": profile,
        "status": "READY",
        "platformRevision": revision,
        "host": _host_projection(wsl, hard_isolation=not (wsl or dev_mode or disable_device_bpf)),
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


def _component_readiness_code(name: str, record: dict[str, Any], layout: Layout) -> str | None:
    """Recheck a recorded component's process and every owned UDS endpoint.

    中文:复核已记录组件的进程身份及其每个自有 UDS endpoint。
    """

    paths = _component_socket_paths(layout).get(name)
    pid = record.get("pid")
    start_time = record.get("startTime")
    if paths is None or type(pid) is not int or type(start_time) is not int:
        return "RUNTIME_STATE_INVALID"
    if record.get("socket") != str(paths[0]) or not _alive(record):
        return "RUNTIME_COMPONENT_EXITED"
    owned = record.get("ownedSockets")
    if not isinstance(owned, dict):
        return "RUNTIME_STATE_INVALID"
    for path in paths:
        identity = owned.get(str(path))
        if not _is_socket_identity(identity):
            return "RUNTIME_HEALTH_CHECK_FAILED"
        try:
            if _socket_identity(path) != identity:
                return "RUNTIME_HEALTH_CHECK_FAILED"
            if _probe_component_socket(name, path, SOCKET_CONNECT_TIMEOUT_SECONDS) != pid:
                return "RUNTIME_HEALTH_CHECK_FAILED"
        except BootstrapFailure:
            return "RUNTIME_HEALTH_CHECK_FAILED"
    return None if _alive(record) else "RUNTIME_COMPONENT_EXITED"


def up(args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Build and start the canonical adapters and Kernel in dependency order.

    中文:按依赖顺序构建并启动规范 Adapter 和 Kernel。
    """

    if sys.platform != "linux":
        raise BootstrapFailure("RUNTIME_OS_UNSUPPORTED", "The V1 runtime requires Linux")
    wsl = _is_wsl()
    if wsl and args.profile != WSL_DEV_PROFILE:
        raise BootstrapFailure("WSL_PROFILE_REQUIRED", "WSL shared-device mode requires explicit WSL_DEV_PROFILE")
    if not wsl and args.profile == WSL_DEV_PROFILE:
        raise BootstrapFailure("WSL_PROFILE_INVALID", "WSL_DEV_PROFILE is accepted only on WSL2")
    build_from_source = getattr(args, "build_from_source", False)
    no_build = getattr(args, "no_build", False)
    if build_from_source and no_build:
        raise BootstrapFailure("RUNTIME_BUILD_MODE_INVALID", "--build-from-source conflicts with --no-build")
    existing = _load_processes(layout)
    if any(_alive(record) for record in existing.values()):
        raise BootstrapFailure("RUNTIME_ALREADY_RUNNING", "Use runtime status or down first")
    _remove_owned_sockets(layout, existing)
    if existing:
        _atomic_json(layout.processes, {})

    root = _platform_root()
    revision = _platform_revision(root)
    _prepare_signing_key(layout)
    binaries = _build_binaries(root, layout) if build_from_source else _installed_binaries(layout)
    uid = os.getuid()
    socket_paths = _component_socket_paths(layout)
    sockets = {name: paths[0] for name, paths in socket_paths.items()}
    dev_mode = getattr(args, "dev_mode", False) or wsl
    disable_device_bpf = getattr(args, "disable_device_bpf", False) or dev_mode
    commands = {
        "systemAdapter": [
            str(binaries["systemAdapter"]),
            "--socket",
            str(sockets["systemAdapter"]),
            "--allowed-client-uid",
            str(uid),
            "--adapter-id",
            "linux-system",
        ],
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
            *(["--dev-mode"] if dev_mode else []),
            *(["--disable-device-bpf"] if disable_device_bpf and not dev_mode else []),
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
            "--system-adapter",
            "linux-system=" + str(sockets["systemAdapter"]),
            "--system-adapter-peer-uid",
            "linux-system=" + str(uid),
            "--hardware-adapter",
            "nvidia-smi=" + str(sockets["nvidiaAdapter"]),
            "--hardware-adapter-peer-uid",
            "nvidia-smi=" + str(uid),
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
            _process, record = _start_component(name, commands[name], socket_paths[name], layout)
            processes[name] = record
            _atomic_json(layout.processes, processes)
        gpu = _nvidia_probe()
        manifest = _manifest(
            layout=layout,
            revision=revision,
            profile=args.profile,
            wsl=wsl,
            gpu=gpu,
            processes=processes,
            binaries=binaries,
            dev_mode=dev_mode,
            disable_device_bpf=disable_device_bpf,
        )
        # A node can serve CPU-bound work without an NVIDIA device. GPU work must
        # pass binding-aware preflight and execution readiness at submission time.
        for name in COMPONENT_ORDER:
            code = _component_readiness_code(name, processes[name], layout)
            if code is not None:
                raise BootstrapFailure(code, "A runtime component failed its final readiness check")
        _atomic_json(layout.manifest, manifest)
        return _public_evidence(manifest)
    except Exception as startup_error:
        failed_record = getattr(startup_error, "process_record", None)
        failed_handle = getattr(startup_error, "process_handle", None)
        if isinstance(failed_record, tuple) and len(failed_record) == 2:
            name, record = failed_record
            if isinstance(name, str) and isinstance(record, dict):
                processes[name] = record
        try:
            if isinstance(failed_handle, subprocess.Popen):
                _stop_process(failed_handle)
            _stop_records(processes)
            _remove_owned_sockets(layout, processes)
            _atomic_json(layout.processes, {})
        except Exception as cleanup_error:
            if processes:
                _atomic_json(layout.processes, processes)
            raise cleanup_error from startup_error
        raise


def status(_args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Read exact process identities and report sanitized component health.

    中文:读取准确的进程身份,并报告经过净化的组件健康状态。
    """

    processes = _load_processes(layout)
    states = {
        name: "READY"
        if _component_readiness_code(name, processes.get(name, {}), layout) is None
        else "UNAVAILABLE"
        for name in COMPONENT_ORDER
    }
    manifest: dict[str, Any]
    try:
        manifest = json.loads(layout.manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        manifest = {
            "profile": PROFILE,
            "runtimeMode": None,
            "platformRevision": None,
            "host": _host_projection(_is_wsl(), hard_isolation=False),
            "gpu": None,
        }
    manifest["status"] = "READY" if all(value == "READY" for value in states.values()) else "DOWN"
    manifest["components"] = {name: {"status": value} for name, value in states.items()}
    return _public_evidence(manifest)


def down(_args: argparse.Namespace, layout: Layout) -> dict[str, Any]:
    """Gracefully stop owned processes and clear only owned UDS files.

    中文:优雅地停止受管理进程,并且只清理本流程拥有的 UDS 文件。
    """

    processes = _capture_owned_socket_identities(layout, _load_processes(layout))
    if processes:
        _atomic_json(layout.processes, processes)
    forced = _stop_records(processes)
    _remove_owned_sockets(layout, processes)
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
    """Define the stable bootstrap command surface.

    中文:定义稳定的 bootstrap 命令接口。
    """

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
    build_mode = value.add_mutually_exclusive_group()
    build_mode.add_argument(
        "--build-from-source",
        action="store_true",
        help="Developer-only: build native components from this checkout with Cargo",
    )
    build_mode.add_argument(
        "--no-build",
        action="store_true",
        help="Compatibility flag: require the preinstalled binaries in runtime-home/bin",
    )
    value.add_argument(
        "--dev-mode",
        action="store_true",
        help="Run sandboxd's non-isolated development path; readiness will not claim hard isolation",
    )
    value.add_argument(
        "--disable-device-bpf",
        action="store_true",
        help="Disable device BPF while retaining sandboxd cgroup mode; readiness will not claim hard isolation",
    )
    return value


def main() -> int:
    """Execute one locked runtime operation and print sanitized JSON only.

    中文:执行一项受锁保护的 runtime 操作,并且只输出经过净化的 JSON。
    """

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
