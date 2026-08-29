//! Cgroup v2 configuration and cleanup reporting.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
use std::{fs, path::Path};

use cy_kernel_api::CapabilityFact;
#[cfg(target_os = "linux")]
use cy_kernel_api::ProviderError;

pub(crate) const OWNED_INSTANCE_PREFIX: &str = "instance-";

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
        let path = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "CGROUP_V2_NOT_MOUNTED",
                    "missing unified cgroup membership",
                )
            })?;
        Ok(Path::new("/sys/fs/cgroup")
            .join(path.trim_start_matches('/'))
            .join(child_name))
    }
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
