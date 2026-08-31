// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/sys.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Low-level system, file I/O, process, and pidfd helpers.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};

pub(crate) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

pub(crate) fn read_key_values(path: impl AsRef<Path>) -> BTreeMap<String, u64> {
    read_trimmed(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key.to_string(), value.parse().ok()?))
        })
        .collect()
}

pub fn read_oom_kill_count(cgroup_path: &Path) -> u64 {
    read_key_values(cgroup_path.join("memory.events.local"))
        .get("oom_kill")
        .copied()
        .unwrap_or(0)
}

pub(crate) fn wait_until_empty(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if read_trimmed(path.join("cgroup.procs"))
            .map(|value| value.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn open_pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open has no pointer arguments. The returned descriptor is
    // immediately transferred into OwnedFd, which closes it exactly once.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    (fd >= 0).then(|| {
        // SAFETY: fd is a newly returned, owned descriptor from pidfd_open.
        unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) }
    })
}

pub(crate) fn pidfd_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        open_pidfd(std::process::id()).is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub(crate) fn proc_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()?
            .split_whitespace()
            .nth(21)?
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}
