"""Test shared RuntimeDirectory cleanup with isolated transient systemd units.

This test touches only random user units and subdirectories under XDG_RUNTIME_DIR.
中文：只用随机命名的 user unit 验证共享运行时目录清理行为，不操作已安装服务。
"""

from __future__ import annotations

import os
import platform
import secrets
import shutil
import socket
import stat
import subprocess
import sys
import time
from pathlib import Path


class LifecycleFailure(RuntimeError):
    """Raised when the isolated shared-directory lifecycle differs from contract."""


def _run(command: list[str], *, check: bool = True, timeout: float = 15) -> subprocess.CompletedProcess[str]:
    """Run a systemd command and include its captured output in failures."""

    result = subprocess.run(command, capture_output=True, check=False, text=True, timeout=timeout)
    if check and result.returncode != 0:
        raise LifecycleFailure(
            f"command failed ({result.returncode}): {' '.join(command)}\n{result.stdout}\n{result.stderr}"
        )
    return result


def _preflight() -> str | None:
    """Return a skip reason unless a Linux per-user systemd manager is available."""

    if platform.system() != "Linux":
        return "requires Linux"
    if shutil.which("systemctl") is None or shutil.which("systemd-run") is None:
        return "systemctl and systemd-run are required"
    runtime_root = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime_root or not Path(runtime_root).is_dir():
        return "XDG_RUNTIME_DIR is unavailable"
    probe = _run(["systemctl", "--user", "show-environment"], check=False)
    if probe.returncode != 0:
        return f"systemd user manager is unavailable: {probe.stderr.strip()}"
    return None


def _serve() -> int:
    """Bind a test Unix socket and keep its service process alive."""

    runtime_directory = os.environ.get("RUNTIME_DIRECTORY")
    if not runtime_directory:
        raise LifecycleFailure("systemd did not set RUNTIME_DIRECTORY")
    socket_path = Path(runtime_directory) / "kernel.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(socket_path))
    server.listen(8)
    server.settimeout(0.25)
    try:
        while True:
            try:
                connection, _ = server.accept()
            except TimeoutError:
                continue
            with connection:
                connection.sendall(b"kernel-ok\n")
    finally:
        server.close()
        if socket_path.exists():
            socket_path.unlink()


def _systemctl(*arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    """Run an operation against the per-user manager, never the system manager."""

    return _run(["systemctl", "--user", *arguments], check=check)


def _start_unit(unit: str, runtime_directory: str, preserve: str, command: list[str]) -> None:
    """Start a random transient unit with an independent declaration of one shared directory."""

    _run(
        [
            "systemd-run",
            "--user",
            f"--unit={unit}",
            f"--property=RuntimeDirectory={runtime_directory}",
            "--property=RuntimeDirectoryMode=0700",
            f"--property=RuntimeDirectoryPreserve={preserve}",
            "--",
            *command,
        ]
    )


def _main_pid(unit: str) -> int:
    """Read the current MainPID, returning zero while systemd activates the unit."""

    result = _systemctl("show", unit, "--property=MainPID", "--value", check=False)
    if result.returncode != 0:
        return 0
    try:
        return int(result.stdout.strip())
    except ValueError as error:
        raise LifecycleFailure(f"unit {unit} returned invalid MainPID {result.stdout!r}") from error


def _connect(socket_path: Path) -> bool:
    """Connect to the socket and require a response from the still-live server."""

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(0.5)
            client.connect(str(socket_path))
            return client.recv(32) == b"kernel-ok\n"
    except OSError:
        return False


def _wait_for_server(unit: str, socket_path: Path) -> int:
    """Wait for the socket listener and capture its process identity."""

    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        pid = _main_pid(unit)
        if pid > 0 and _connect(socket_path):
            return pid
        time.sleep(0.05)
    raise LifecycleFailure(f"unit {unit} did not expose a responding socket at {socket_path}")


def _assert_server_alive(unit: str, socket_path: Path, expected_pid: int) -> None:
    """Confirm the server process and its bound pathname survive a peer stop."""

    active = _systemctl("is-active", unit, check=False)
    actual_pid = _main_pid(unit)
    if active.returncode != 0 or actual_pid != expected_pid:
        raise LifecycleFailure(
            f"server changed state or PID: active={active.stdout.strip()} "
            f"expected_pid={expected_pid} actual_pid={actual_pid}"
        )
    try:
        socket_type = stat.S_ISSOCK(socket_path.lstat().st_mode)
    except FileNotFoundError:
        socket_type = False
    if not socket_type or not _connect(socket_path):
        raise LifecycleFailure(f"server socket did not survive peer stop: {socket_path}")


def _stop_unit(unit: str) -> None:
    """Stop one exact transient service created by this run."""

    _systemctl("stop", unit, check=False)


def _cleanup_runtime_directory(runtime_path: Path, runtime_root: Path) -> None:
    """Remove only this run's expected directory and socket, without recursion."""

    if not runtime_path.exists() and not runtime_path.is_symlink():
        return
    if runtime_path.parent.resolve() != runtime_root.resolve() or runtime_path.is_symlink():
        raise LifecycleFailure(f"refusing to clean unexpected runtime path {runtime_path}")
    if not stat.S_ISDIR(runtime_path.lstat().st_mode):
        raise LifecycleFailure(f"refusing to clean non-directory test path {runtime_path}")
    for child in runtime_path.iterdir():
        metadata = child.lstat()
        if child.name != "kernel.sock" or not stat.S_ISSOCK(metadata.st_mode):
            raise LifecycleFailure(f"unexpected entry in isolated test directory: {child}")
        child.unlink()
    runtime_path.rmdir()


def _run_scenario(
    prefix: str,
    scenario: str,
    *,
    peer_preserve: str,
    expect_preserved: bool,
    created_units: list[str],
    runtime_paths: list[Path],
) -> None:
    """Compare the official preserve policy with an unpreserved expected-failure control."""

    runtime_directory = f"{prefix}-{scenario}-runtime"
    server_unit = f"{prefix}-{scenario}-server.service"
    peer_unit = f"{prefix}-{scenario}-peer.service"
    _start_unit(
        server_unit,
        runtime_directory,
        "yes",
        [sys.executable, str(Path(__file__).resolve()), "--serve"],
    )
    created_units.append(server_unit)
    socket_path = Path(os.environ["XDG_RUNTIME_DIR"]) / runtime_directory / "kernel.sock"
    runtime_paths.append(socket_path.parent)
    server_pid = _wait_for_server(server_unit, socket_path)

    _start_unit(peer_unit, runtime_directory, peer_preserve, ["/usr/bin/sleep", "infinity"])
    created_units.append(peer_unit)
    _systemctl("is-active", peer_unit)
    _stop_unit(peer_unit)
    if expect_preserved:
        _assert_server_alive(server_unit, socket_path, server_pid)
        _start_unit(peer_unit, runtime_directory, peer_preserve, ["/usr/bin/sleep", "infinity"])
        _systemctl("is-active", peer_unit)
        _stop_unit(peer_unit)
        _assert_server_alive(server_unit, socket_path, server_pid)
        print(f"PASS {scenario}: peer stop/start preserved server PID {server_pid} and its socket")
        return

    _systemctl("is-active", server_unit)
    if _main_pid(server_unit) != server_pid:
        raise LifecycleFailure("unpreserved control unexpectedly changed the server PID")
    if socket_path.exists() or socket_path.parent.exists():
        raise LifecycleFailure("unpreserved control did not remove the shared runtime directory")
    print(f"PASS {scenario}: unpreserved peer removed the pathname while server PID {server_pid} stayed active")


def _run_lifecycle() -> None:
    """Run both isolated cases, then stop and clean only generated units and paths."""

    runtime_root = Path(os.environ["XDG_RUNTIME_DIR"]).resolve()
    prefix = f"cyrene-runtime-dir-test-{os.getuid()}-{os.getpid()}-{secrets.token_hex(6)}"
    created_units: list[str] = []
    runtime_paths: list[Path] = []
    try:
        _run_scenario(
            prefix,
            "preserved",
            peer_preserve="yes",
            expect_preserved=True,
            created_units=created_units,
            runtime_paths=runtime_paths,
        )
        _run_scenario(
            prefix,
            "unpreserved-control",
            peer_preserve="no",
            expect_preserved=False,
            created_units=created_units,
            runtime_paths=runtime_paths,
        )
    finally:
        for unit in reversed(created_units):
            _stop_unit(unit)
            _systemctl("reset-failed", unit, check=False)
        for runtime_path in reversed(runtime_paths):
            _cleanup_runtime_directory(runtime_path, runtime_root)


def main() -> int:
    """Run the isolated systemd regression or return pytest's skip code."""

    if len(sys.argv) == 2 and sys.argv[1] == "--serve":
        return _serve()
    reason = _preflight()
    if reason is not None:
        print(f"SKIP: {reason}")
        return 77
    try:
        _run_lifecycle()
    except LifecycleFailure as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print("PASS: isolated systemd shared-runtime lifecycle regression")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
