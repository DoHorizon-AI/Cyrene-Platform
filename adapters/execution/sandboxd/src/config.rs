// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/config.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Cgroup v2 configuration and cleanup reporting.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
use std::{fs, path::Path};

use cy_kernel_api::CapabilityFact;
#[cfg(target_os = "linux")]
use cy_kernel_api::ProviderError;

pub(crate) const OWNED_INSTANCE_PREFIX: &str = "instance-";
#[cfg(target_os = "linux")]
pub(crate) const DELEGATED_PROCESS_LEAF: &str = "sandboxd-control";

/// Cgroup v2 configuration. `root` must be a dedicated, systemd-delegated
/// CYRENE subtree such as `/sys/fs/cgroup/.../cyrene`, not `/sys/fs/cgroup`.
#[derive(Debug, Clone)]
pub struct CgroupV2Config {
    pub root: PathBuf,
    pub transport_root: PathBuf,
    /// Enables loading and attaching real `BPF_PROG_TYPE_CGROUP_DEVICE` filters.
    /// A failed program load or attach always rejects a HARD launch.
    pub device_bpf_enabled: bool,
    /// Development fallback mode for non-root / non-cgroup environments.
    pub dev_mode: bool,
}

impl CgroupV2Config {
    pub fn host_default() -> Self {
        Self {
            root: PathBuf::from("/sys/fs/cgroup/cyrene"),
            transport_root: PathBuf::from("/run/cyrene/workers"),
            device_bpf_enabled: true,
            dev_mode: false,
        }
    }

    /// Resolves a child of this process's own delegated cgroup. On a systemd
    /// host this avoids guessing `system.slice` paths and makes the parent
    /// boundary explicit before any cleanup is attempted.
    #[cfg(target_os = "linux")]
    pub fn delegated_root(child_name: &str) -> Result<PathBuf, ProviderError> {
        if !safe_cgroup_segment(child_name) {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_NOT_OWNED",
                "delegated root child name is invalid",
            ));
        }
        let membership = fs::read_to_string("/proc/self/cgroup").map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_MEMBERSHIP_READ_FAILED",
                &error.to_string(),
            )
        })?;
        let (service_group, _) = delegated_group_paths(&membership)?;
        Ok(service_group.join(child_name))
    }
}

#[cfg(target_os = "linux")]
pub(crate) trait DelegatedCgroupIo {
    fn current_pid(&self) -> u32;
    fn current_group(&mut self) -> Result<PathBuf, ProviderError>;
    fn group_exists(&mut self, group: &Path) -> Result<bool, ProviderError>;
    fn direct_processes(&mut self, group: &Path) -> Result<Vec<u32>, ProviderError>;
    fn child_groups(&mut self, group: &Path) -> Result<Vec<PathBuf>, ProviderError>;
    fn ensure_control_group(&mut self, parent: &Path) -> Result<PathBuf, ProviderError>;
    fn move_process(&mut self, pid: u32, destination: &Path) -> Result<(), ProviderError>;
}

#[cfg(target_os = "linux")]
pub(crate) fn prepare_delegated_process_with<T>(
    worker_root: &Path,
    io: &mut impl DelegatedCgroupIo,
    initialize: impl FnOnce(&mut dyn DelegatedCgroupIo, &Path, &Path) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let Some(parent) = worker_root.parent() else {
        return Err(delegated_error(
            "CGROUP_DELEGATION_PATH_INVALID",
            "worker root must be a child of the delegated service cgroup",
        ));
    };
    if !safe_absolute_cgroup_path(parent)
        || !safe_cgroup_segment(
            worker_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    {
        return Err(delegated_error(
            "CGROUP_DELEGATION_PATH_INVALID",
            "worker root path is not a safe absolute delegated child",
        ));
    }

    let pid = io.current_pid();
    if pid == 0 {
        return Err(delegated_error(
            "CGROUP_DELEGATION_PID_INVALID",
            "current process ID is invalid",
        ));
    }
    let control_group = parent.join(DELEGATED_PROCESS_LEAF);
    let current = io.current_group()?;
    if current != parent && current != control_group {
        return Err(delegated_error(
            "CGROUP_DELEGATION_PROBE_MISMATCH",
            "current process is outside the expected delegated service or control leaf",
        ));
    }

    validate_delegated_children(io, parent, worker_root, &control_group)?;
    if io.group_exists(worker_root)? && !io.direct_processes(worker_root)?.is_empty() {
        return Err(delegated_error(
            "CGROUP_DELEGATION_WORKER_ROOT_POPULATED",
            "worker root contains direct processes",
        ));
    }

    let parent_processes = io.direct_processes(parent)?;
    if current == parent {
        if parent_processes != [pid] {
            return Err(delegated_error(
                "CGROUP_DELEGATION_PARENT_POPULATED",
                "service cgroup contains a process other than the current daemon",
            ));
        }
        let leaf = io.ensure_control_group(parent)?;
        if leaf != control_group
            || !io.direct_processes(&leaf)?.is_empty()
            || !io.child_groups(&leaf)?.is_empty()
        {
            return Err(delegated_error(
                "CGROUP_DELEGATION_CONTROL_LEAF_UNSAFE",
                "control leaf is not empty or has an unexpected path",
            ));
        }
        io.move_process(pid, &leaf)?;
    } else if !parent_processes.is_empty()
        || io.direct_processes(&control_group)? != [pid]
        || !io.child_groups(&control_group)?.is_empty()
    {
        return Err(delegated_error(
            "CGROUP_DELEGATION_CONTROL_LEAF_UNSAFE",
            "existing control leaf does not contain only the current daemon",
        ));
    }

    if io.current_group()? != control_group
        || io.direct_processes(&control_group)? != [pid]
        || !io.direct_processes(parent)?.is_empty()
    {
        return Err(delegated_error(
            "CGROUP_DELEGATION_VERIFY_FAILED",
            "daemon move did not leave the service cgroup empty",
        ));
    }
    validate_delegated_children(io, parent, worker_root, &control_group)?;
    initialize(io, parent, worker_root)
}

#[cfg(target_os = "linux")]
fn validate_delegated_children(
    io: &mut impl DelegatedCgroupIo,
    parent: &Path,
    worker_root: &Path,
    control_group: &Path,
) -> Result<(), ProviderError> {
    for child in io.child_groups(parent)? {
        if child != worker_root && child != control_group {
            return Err(delegated_error(
                "CGROUP_DELEGATION_CHILD_UNEXPECTED",
                "delegated service cgroup contains an unexpected child group",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn delegated_group_paths(membership: &str) -> Result<(PathBuf, PathBuf), ProviderError> {
    let mut unified = membership
        .lines()
        .filter_map(|line| line.strip_prefix("0::"));
    let Some(group) = unified.next() else {
        return Err(delegated_error(
            "CGROUP_V2_NOT_MOUNTED",
            "missing unified cgroup membership",
        ));
    };
    if unified.next().is_some() || !safe_absolute_cgroup_path(Path::new(group)) {
        return Err(delegated_error(
            "CGROUP_MEMBERSHIP_INVALID",
            "unified cgroup membership is duplicated or malformed",
        ));
    }

    let mount = Path::new("/sys/fs/cgroup");
    let current = mount.join(group.trim_start_matches('/'));
    let service =
        if current.file_name().and_then(|name| name.to_str()) == Some(DELEGATED_PROCESS_LEAF) {
            current.parent().ok_or_else(|| {
                delegated_error(
                    "CGROUP_MEMBERSHIP_INVALID",
                    "control leaf has no delegated service parent",
                )
            })?
        } else {
            current.as_path()
        };
    if service == mount || !service.starts_with(mount) {
        return Err(delegated_error(
            "CGROUP_ROOT_NOT_OWNED",
            "current cgroup is not a named delegated service group",
        ));
    }
    Ok((service.to_path_buf(), current))
}

#[cfg(target_os = "linux")]
fn safe_absolute_cgroup_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

#[cfg(target_os = "linux")]
fn delegated_error(code: &'static str, message: impl AsRef<str>) -> ProviderError {
    ProviderError::new("linux-cgroup-v2", code, message.as_ref())
}

#[cfg(target_os = "linux")]
pub(crate) struct LinuxDelegatedCgroupIo;

#[cfg(target_os = "linux")]
impl LinuxDelegatedCgroupIo {
    fn validate_group(path: &Path) -> Result<(), ProviderError> {
        let mount = Path::new("/sys/fs/cgroup");
        if !safe_absolute_cgroup_path(path) || !path.starts_with(mount) {
            return Err(delegated_error(
                "CGROUP_DELEGATION_PATH_INVALID",
                "cgroup path is outside the unified cgroup mount",
            ));
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            delegated_error("CGROUP_DELEGATION_PATH_UNAVAILABLE", error.to_string())
        })?;
        let canonical = fs::canonicalize(path).map_err(|error| {
            delegated_error("CGROUP_DELEGATION_PATH_UNAVAILABLE", error.to_string())
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || canonical != path {
            return Err(delegated_error(
                "CGROUP_DELEGATION_PATH_UNSAFE",
                "cgroup path is not a canonical directory",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl DelegatedCgroupIo for LinuxDelegatedCgroupIo {
    fn current_pid(&self) -> u32 {
        std::process::id()
    }

    fn current_group(&mut self) -> Result<PathBuf, ProviderError> {
        let membership = fs::read_to_string("/proc/self/cgroup")
            .map_err(|error| delegated_error("CGROUP_MEMBERSHIP_READ_FAILED", error.to_string()))?;
        let (group, current) = delegated_group_paths(&membership)?;
        if current != group && current.parent() != Some(group.as_path()) {
            return Err(delegated_error(
                "CGROUP_MEMBERSHIP_INVALID",
                "current membership does not match its delegated service group",
            ));
        }
        Self::validate_group(&current)?;
        Ok(current)
    }

    fn group_exists(&mut self, group: &Path) -> Result<bool, ProviderError> {
        match fs::symlink_metadata(group) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(delegated_error(
                        "CGROUP_DELEGATION_PATH_UNSAFE",
                        "expected cgroup child is not a real directory",
                    ));
                }
                Self::validate_group(group)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(delegated_error(
                "CGROUP_DELEGATION_PATH_UNAVAILABLE",
                error.to_string(),
            )),
        }
    }

    fn direct_processes(&mut self, group: &Path) -> Result<Vec<u32>, ProviderError> {
        Self::validate_group(group)?;
        let path = group.join("cgroup.procs");
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            delegated_error("CGROUP_PROCESS_STATE_READ_FAILED", error.to_string())
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(delegated_error(
                "CGROUP_PROCESS_STATE_INVALID",
                "cgroup.procs is not a regular cgroup file",
            ));
        }
        let contents = fs::read_to_string(path).map_err(|error| {
            delegated_error("CGROUP_PROCESS_STATE_READ_FAILED", error.to_string())
        })?;
        parse_cgroup_process_ids(&contents)
    }

    fn child_groups(&mut self, group: &Path) -> Result<Vec<PathBuf>, ProviderError> {
        Self::validate_group(group)?;
        let entries = fs::read_dir(group)
            .map_err(|error| delegated_error("CGROUP_CHILD_DISCOVERY_FAILED", error.to_string()))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                delegated_error("CGROUP_CHILD_DISCOVERY_FAILED", error.to_string())
            })?;
            let file_type = entry.file_type().map_err(|error| {
                delegated_error("CGROUP_CHILD_DISCOVERY_FAILED", error.to_string())
            })?;
            if file_type.is_symlink() {
                return Err(delegated_error(
                    "CGROUP_DELEGATION_CHILD_UNSAFE",
                    "delegated cgroup contains a symbolic link",
                ));
            }
            if file_type.is_dir() {
                let child = entry.path();
                Self::validate_group(&child)?;
                children.push(child);
            } else if !file_type.is_file() {
                return Err(delegated_error(
                    "CGROUP_DELEGATION_CHILD_UNSAFE",
                    "delegated cgroup contains an unsupported filesystem entry",
                ));
            }
        }
        Ok(children)
    }

    fn ensure_control_group(&mut self, parent: &Path) -> Result<PathBuf, ProviderError> {
        Self::validate_group(parent)?;
        let child = parent.join(DELEGATED_PROCESS_LEAF);
        match fs::create_dir(&child) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(delegated_error(
                    "CGROUP_DELEGATION_CONTROL_CREATE_FAILED",
                    error.to_string(),
                ));
            }
        }
        Self::validate_group(&child)?;
        Ok(child)
    }

    fn move_process(&mut self, pid: u32, destination: &Path) -> Result<(), ProviderError> {
        Self::validate_group(destination)?;
        let path = destination.join("cgroup.procs");
        fs::write(path, pid.to_string())
            .map_err(|error| delegated_error("CGROUP_DELEGATION_MOVE_FAILED", error.to_string()))
    }
}

#[cfg(target_os = "linux")]
fn parse_cgroup_process_ids(contents: &str) -> Result<Vec<u32>, ProviderError> {
    let mut processes = Vec::new();
    for value in contents.split_whitespace() {
        let pid = value.parse::<u32>().map_err(|_| {
            delegated_error(
                "CGROUP_PROCESS_STATE_INVALID",
                "cgroup.procs contains an invalid process ID",
            )
        })?;
        if pid == 0 || processes.contains(&pid) {
            return Err(delegated_error(
                "CGROUP_PROCESS_STATE_INVALID",
                "cgroup.procs contains a zero or duplicate process ID",
            ));
        }
        processes.push(pid);
    }
    processes.sort_unstable();
    Ok(processes)
}

/// Outcome of startup cleanup, kept explicit for an operator-visible startup
/// gate rather than silently deleting cgroups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnedCgroupCleanupReport {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<PathBuf>,
}

pub(crate) fn is_owned_instance_name(name: &str) -> bool {
    name.starts_with(OWNED_INSTANCE_PREFIX)
        && name.len() > OWNED_INSTANCE_PREFIX.len()
        && safe_cgroup_segment(name)
}

pub(crate) fn safe_cgroup_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(crate) fn fact(name: &str, available: bool, required: bool, detail: &str) -> CapabilityFact {
    CapabilityFact {
        name: name.to_string(),
        available,
        required,
        detail: detail.to_string(),
    }
}
