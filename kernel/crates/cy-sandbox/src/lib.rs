//! CYRENE Linux cgroup v2 sandbox runtime.
//!
//! The configured root is an **owned and delegated** cgroup subtree, never the
//! cgroup mount root. Startup reaps only direct instance cgroups below that
//! subtree; this is the boundary that makes Fate Sharing cleanup safe.

#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use cy_kernel_api::{
    CapabilityFact, CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, DeviceMapper,
    EnforcementMode, EnforcementReport, LaunchPlan, NodeCapabilities, ProcessCondition,
    ProcessHandle, ProcessRuntime, ProviderError, SandboxBackend, StopRequest,
};

mod managed_process;
pub use managed_process::{SandboxedProcess, SandboxedProcessState};

const CPU_PERIOD_USEC: u64 = 100_000;
const OWNED_INSTANCE_PREFIX: &str = "instance-";

/// Cgroup v2 configuration. `root` must be a dedicated, systemd-delegated
/// CYRENE subtree such as `/sys/fs/cgroup/.../cyrene`, not `/sys/fs/cgroup`.
#[derive(Debug, Clone)]
pub struct CgroupV2Config {
    pub root: PathBuf,
    /// Enables loading and attaching real `BPF_PROG_TYPE_CGROUP_DEVICE` filters.
    /// A failed program load or attach always rejects a HARD launch.
    pub device_bpf_enabled: bool,
}

impl CgroupV2Config {
    pub fn host_default() -> Self {
        Self {
            root: PathBuf::from("/sys/fs/cgroup/cyrene"),
            device_bpf_enabled: true,
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

/// Linux cgroup v2 execution backend. The `children` map exists solely for
/// bounded reaping; cgroup.kill remains the authority for the whole tree.
#[derive(Debug)]
pub struct CgroupV2Runtime {
    config: CgroupV2Config,
    children: Mutex<BTreeMap<u32, Child>>,
}

impl CgroupV2Runtime {
    pub fn new(config: CgroupV2Config) -> Self {
        Self {
            config,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// Creates and validates the dedicated root, enables available controllers
    /// on its parent, then kills only direct CYRENE instance cgroups underneath.
    /// This must run once during the daemon composition-root startup.
    pub fn initialize_owned_root(&self) -> Result<OwnedCgroupCleanupReport, ProviderError> {
        self.validate_owned_root()?;
        fs::create_dir_all(&self.config.root).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_CREATE_FAILED",
                &error.to_string(),
            )
        })?;
        self.enable_parent_controllers()?;
        self.cleanup_owned_instances()
    }

    /// Validates a direct instance group name and resolves it beneath the owned root.
    pub fn cgroup_path(&self, name: &str) -> Result<PathBuf, ProviderError> {
        if !is_owned_instance_name(name) {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "INVALID_CGROUP_NAME",
                "instance cgroups must be a safe instance-* direct child",
            ));
        }
        Ok(self.config.root.join(name))
    }

    /// Reads the cgroup v2 physical counters used by the supervisor and
    /// telemetry exporters. Missing optional files stay `None`; no estimate is
    /// substituted for a kernel counter.
    pub fn telemetry(&self, handle: &ProcessHandle) -> CgroupTelemetry {
        let cpu = read_key_values(handle.cgroup_path.join("cpu.stat"));
        CgroupTelemetry {
            memory_current_bytes: read_trimmed(handle.cgroup_path.join("memory.current"))
                .and_then(|value| value.parse().ok()),
            memory_peak_bytes: read_trimmed(handle.cgroup_path.join("memory.peak"))
                .and_then(|value| value.parse().ok()),
            cpu_usage_usec: cpu.get("usage_usec").copied(),
            cpu_user_usec: cpu.get("user_usec").copied(),
            cpu_system_usec: cpu.get("system_usec").copied(),
            oom_kill_count: read_oom_kill_count(&handle.cgroup_path),
        }
    }

    pub fn preflight_report(&self) -> NodeCapabilities {
        let controllers = read_trimmed(self.config.root.join("cgroup.controllers"));
        let cgroup_v2 = controllers.is_some();
        let has_controller = |name: &str| {
            controllers
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .any(|controller| controller == name)
        };
        let cgroup_kill = self.config.root.join("cgroup.kill").is_file();
        let facts = vec![
            fact("cgroup-v2", cgroup_v2, true, "owned cgroup.controllers"),
            fact("cgroup-kill", cgroup_kill, true, "owned cgroup.kill"),
            fact(
                "memory-controller",
                has_controller("memory"),
                true,
                "memory",
            ),
            fact("pids-controller", has_controller("pids"), true, "pids"),
            fact("cpu-controller", has_controller("cpu"), true, "cpu"),
            fact(
                "pidfd",
                pidfd_available(),
                false,
                "pidfd_open feature probe",
            ),
            fact(
                "device-bpf-enabled",
                self.config.device_bpf_enabled,
                false,
                "HARD binding performs a real load and attach at launch",
            ),
        ];
        let ready = facts
            .iter()
            .filter(|fact| fact.required)
            .all(|fact| fact.available);
        NodeCapabilities {
            ready,
            facts,
            enforcement: vec![EnforcementReport {
                resource_kind: "process-tree".to_string(),
                mode: if ready {
                    EnforcementMode::Hard
                } else {
                    EnforcementMode::Unenforced
                },
                adapter_id: "linux-cgroup-v2".to_string(),
                reason_code: if ready {
                    "CGROUP_V2_READY".to_string()
                } else {
                    "CGROUP_V2_PREREQUISITES_MISSING".to_string()
                },
            }],
        }
    }

    fn validate_owned_root(&self) -> Result<(), ProviderError> {
        let Some(name) = self
            .config
            .root
            .file_name()
            .and_then(|value| value.to_str())
        else {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_NOT_OWNED",
                "root must be a named delegated CYRENE subtree",
            ));
        };
        if name.is_empty() || name == "." || name == ".." {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_NOT_OWNED",
                "root must not be the cgroup mount root",
            ));
        }
        Ok(())
    }

    fn enable_parent_controllers(&self) -> Result<(), ProviderError> {
        let Some(parent) = self.config.root.parent() else {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_NOT_OWNED",
                "owned root has no parent",
            ));
        };
        let subtree = parent.join("cgroup.subtree_control");
        if !subtree.exists() {
            // Unit tests use a synthetic cgroup filesystem. A real kernel root
            // always has this file, so launch/preflight still gates production.
            return Ok(());
        }
        let available = read_trimmed(parent.join("cgroup.controllers")).unwrap_or_default();
        let requested = ["cpu", "memory", "pids", "cpuset"]
            .into_iter()
            .filter(|controller| {
                available
                    .split_whitespace()
                    .any(|value| value == *controller)
            })
            .map(|controller| format!("+{controller}"))
            .collect::<Vec<_>>();
        if requested.is_empty() {
            return Ok(());
        }
        fs::write(&subtree, requested.join(" ")).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_CONTROLLER_ENABLE_FAILED",
                &error.to_string(),
            )
        })
    }

    fn cleanup_owned_instances(&self) -> Result<OwnedCgroupCleanupReport, ProviderError> {
        let mut report = OwnedCgroupCleanupReport::default();
        for entry in fs::read_dir(&self.config.root).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_READ_FAILED",
                &error.to_string(),
            )
        })? {
            let entry = entry.map_err(|error| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "CGROUP_ROOT_READ_FAILED",
                    &error.to_string(),
                )
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_owned_instance_name(name)
                || !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();
            if !path.join("cgroup.procs").is_file() || !path.join("cgroup.kill").is_file() {
                report.failed.push(path);
                continue;
            }
            if self.kill_cgroup(&path).is_err() || !wait_until_empty(&path, Duration::from_secs(5))
            {
                report.failed.push(path);
                continue;
            }
            if fs::remove_dir(&path).is_ok() {
                report.removed.push(path);
            } else {
                report.failed.push(path);
            }
        }
        if report.failed.is_empty() {
            Ok(report)
        } else {
            Err(ProviderError::new(
                "linux-cgroup-v2",
                "OWNED_CGROUP_CLEANUP_INCOMPLETE",
                &report
                    .failed
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
        }
    }

    fn create_instance_cgroup(&self, name: &str) -> Result<PathBuf, ProviderError> {
        let path = self.cgroup_path(name)?;
        fs::create_dir(&path).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_CREATE_FAILED",
                &error.to_string(),
            )
        })?;
        Ok(path)
    }

    fn apply_limits(&self, cgroup_path: &Path, limits: &CgroupLimits) -> Result<(), ProviderError> {
        if let Some(millicores) = limits.cpu_max_millicores {
            if millicores == 0 {
                return Err(ProviderError::new(
                    "linux-cgroup-v2",
                    "CPU_LIMIT_INVALID",
                    "zero millicores",
                ));
            }
            let quota = u64::from(millicores).saturating_mul(CPU_PERIOD_USEC) / 1_000;
            fs::write(
                cgroup_path.join("cpu.max"),
                format!("{} {}", quota.max(1), CPU_PERIOD_USEC),
            )
            .map_err(|error| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "CPU_LIMIT_APPLY_FAILED",
                    &error.to_string(),
                )
            })?;
        }
        if let Some(bytes) = limits.memory_max_bytes {
            if bytes == 0 {
                return Err(ProviderError::new(
                    "linux-cgroup-v2",
                    "MEMORY_LIMIT_INVALID",
                    "zero bytes",
                ));
            }
            fs::write(cgroup_path.join("memory.max"), bytes.to_string()).map_err(|error| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "MEMORY_LIMIT_APPLY_FAILED",
                    &error.to_string(),
                )
            })?;
        }
        if let Some(cpus) = limits.cpuset_cpus.as_deref() {
            if cpus.is_empty() || cpus.contains('\n') {
                return Err(ProviderError::new(
                    "linux-cgroup-v2",
                    "CPUSET_INVALID",
                    "cpuset must be one non-empty line",
                ));
            }
            fs::write(cgroup_path.join("cpuset.cpus"), cpus).map_err(|error| {
                ProviderError::new("linux-cgroup-v2", "CPUSET_APPLY_FAILED", &error.to_string())
            })?;
        }
        Ok(())
    }

    fn attach(&self, cgroup_path: &Path, pid: u32) -> Result<(), ProviderError> {
        fs::write(cgroup_path.join("cgroup.procs"), pid.to_string()).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ATTACH_FAILED",
                &error.to_string(),
            )
        })
    }

    fn kill_cgroup(&self, cgroup_path: &Path) -> Result<(), ProviderError> {
        fs::write(cgroup_path.join("cgroup.kill"), "1").map_err(|error| {
            ProviderError::new("linux-cgroup-v2", "CGROUP_KILL_FAILED", &error.to_string())
        })
    }

    fn cgroup_is_empty(&self, cgroup_path: &Path) -> bool {
        read_trimmed(cgroup_path.join("cgroup.procs"))
            .map(|value| value.is_empty())
            .unwrap_or(true)
    }

    fn wait_child(&self, pid: u32, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            let result = self.children.lock().ok().and_then(|mut children| {
                children
                    .get_mut(&pid)
                    .and_then(|child| child.try_wait().ok())
                    .flatten()
            });
            if let Some(status) = result {
                if let Ok(mut children) = self.children.lock() {
                    children.remove(&pid);
                }
                return status.code();
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate_pid(&self, pid: u32) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(windows)]
        if let Ok(mut children) = self.children.lock() {
            if let Some(child) = children.get_mut(&pid) {
                let _ = child.kill();
            }
        }
    }

    fn enforce_device_binding(
        &self,
        cgroup_path: &Path,
        binding: &DeviceBinding,
    ) -> Result<(), ProviderError> {
        if binding.enforcement != EnforcementMode::Hard {
            return Ok(());
        }
        if !self.config.device_bpf_enabled {
            return Err(ProviderError::new(
                "linux-device-bpf",
                "HARD_ENFORCEMENT_UNAVAILABLE",
                "device BPF is disabled; a HARD accelerator allocation cannot launch",
            ));
        }
        attach_device_bpf_filter(cgroup_path, binding)
    }
}

impl ProcessRuntime for CgroupV2Runtime {
    fn preflight(&self) -> NodeCapabilities {
        self.preflight_report()
    }

    fn launch(
        &self,
        plan: &LaunchPlan,
        binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError> {
        if !self.preflight_report().ready {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "NODE_NOT_READY",
                "required cgroup v2 capabilities are unavailable",
            ));
        }
        let cgroup_path = self.create_instance_cgroup(&plan.cgroup_name)?;
        if let Err(error) = self
            .apply_limits(&cgroup_path, &plan.limits)
            .and_then(|_| self.enforce_device_binding(&cgroup_path, binding))
        {
            let _ = self.kill_cgroup(&cgroup_path);
            let _ = fs::remove_dir(&cgroup_path);
            return Err(error);
        }
        let environment = binding.merge_environment(&plan.environment)?;
        let mut command = Command::new(&plan.executable);
        command.args(&plan.args).envs(environment);
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let child = command.spawn().map_err(|error| {
            ProviderError::new("native-process", "SPAWN_FAILED", &error.to_string())
        })?;
        let pid = child.id();
        if let Err(error) = self.attach(&cgroup_path, pid) {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            let _ = self.kill_cgroup(&cgroup_path);
            let _ = fs::remove_dir(&cgroup_path);
            return Err(error);
        }
        self.children
            .lock()
            .map_err(|_| ProviderError::new("native-process", "LOCK_POISONED", "child table"))?
            .insert(pid, child);
        Ok(ProcessHandle {
            pid,
            cgroup_path,
            start_time_ticks: proc_start_time(pid),
        })
    }

    fn stop(
        &self,
        handle: &ProcessHandle,
        request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        let before_oom = read_oom_kill_count(&handle.cgroup_path);
        let mut exit_code = None;
        if !request.immediate {
            self.terminate_pid(handle.pid);
            exit_code = self.wait_child(handle.pid, request.grace_period);
        }
        if !self.cgroup_is_empty(&handle.cgroup_path) {
            self.kill_cgroup(&handle.cgroup_path)?;
            if exit_code.is_none() {
                exit_code = self.wait_child(handle.pid, request.grace_period);
            }
        }
        let complete = self.cgroup_is_empty(&handle.cgroup_path)
            && !self
                .children
                .lock()
                .map(|children| children.contains_key(&handle.pid))
                .unwrap_or(true);
        let oom_killed = read_oom_kill_count(&handle.cgroup_path) > before_oom;
        let mut conditions = Vec::new();
        if !complete {
            conditions.push(ProcessCondition {
                reason_code: "REAP_TIMEOUT".to_string(),
                summary: "process or cgroup remained after bounded cleanup".to_string(),
            });
            conditions.push(ProcessCondition {
                reason_code: "CGROUP_NOT_EMPTY".to_string(),
                summary: "resource lease remains held and accelerator is quarantined".to_string(),
            });
        }
        if oom_killed {
            conditions.push(ProcessCondition {
                reason_code: "OOM_KILLED".to_string(),
                summary: "memory.events.local oom_kill counter increased".to_string(),
            });
        }
        if complete {
            let _ = fs::remove_dir(&handle.cgroup_path);
        }
        Ok(CleanupReport {
            complete,
            exit_code,
            oom_killed,
            conditions,
            reason_code: if complete {
                "CLEANUP_COMPLETE".to_string()
            } else {
                "PROCESS_UNINTERRUPTIBLE".to_string()
            },
        })
    }

    fn telemetry(&self, handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
        Ok(CgroupV2Runtime::telemetry(self, handle))
    }
}

impl SandboxBackend for CgroupV2Runtime {
    fn backend_id(&self) -> &str {
        "native-cgroup-v2"
    }
}

/// A policy-only mapper retained for callers that need a pre-launch decision.
/// It no longer reports HARD as applied: only `attach_device_bpf_filter` can do that.
#[derive(Debug, Clone, Copy)]
pub struct LinuxDeviceMapper {
    pub device_bpf_enabled: bool,
}

impl DeviceMapper for LinuxDeviceMapper {
    fn enforce(
        &self,
        binding: &DeviceBinding,
        requested: EnforcementMode,
    ) -> Result<EnforcementReport, ProviderError> {
        if requested == EnforcementMode::Hard && !self.device_bpf_enabled {
            return Err(ProviderError::new(
                "linux-device-bpf",
                "HARD_ENFORCEMENT_UNAVAILABLE",
                "device BPF is disabled",
            ));
        }
        Ok(EnforcementReport {
            resource_kind: "accelerator".to_string(),
            mode: if requested == EnforcementMode::Hard {
                EnforcementMode::ObserveOnly
            } else {
                requested
            },
            adapter_id: binding.adapter_id.clone(),
            reason_code: if requested == EnforcementMode::Hard {
                "HARD_ENFORCEMENT_PENDING_CGROUP_ATTACH".to_string()
            } else {
                "NON_HARD_POLICY".to_string()
            },
        })
    }
}

#[cfg(target_os = "linux")]
fn attach_device_bpf_filter(
    cgroup_path: &Path,
    binding: &DeviceBinding,
) -> Result<(), ProviderError> {
    use std::{fs::File, os::fd::AsRawFd};
    let devices = binding
        .nodes
        .iter()
        .filter_map(device_rule)
        .collect::<Result<Vec<_>, _>>()?;
    if devices.is_empty() {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_RULES_EMPTY",
            "HARD binding contains no valid device nodes",
        ));
    }
    let program = build_device_filter_program(&devices);
    let license = b"GPL\0";
    let mut log = vec![0_u8; 16 * 1024];
    let attr = BpfProgLoadAttr {
        prog_type: BPF_PROG_TYPE_CGROUP_DEVICE,
        insn_cnt: program.len() as u32,
        insns: program.as_ptr() as u64,
        license: license.as_ptr() as u64,
        log_level: 1,
        log_size: log.len() as u32,
        log_buf: log.as_mut_ptr() as u64,
        kern_version: 0,
        prog_flags: 0,
        prog_name: *b"cyrene_devflt\0\0\0",
        prog_ifindex: 0,
        expected_attach_type: BPF_CGROUP_DEVICE,
    };
    let program_fd = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_LOAD,
            &attr,
            std::mem::size_of::<BpfProgLoadAttr>(),
        )
    };
    if program_fd < 0 {
        let verifier_log = String::from_utf8_lossy(&log)
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        let detail = if verifier_log.is_empty() {
            std::io::Error::last_os_error().to_string()
        } else {
            verifier_log
        };
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_BPF_LOAD_FAILED",
            &detail,
        ));
    }
    let cgroup = File::open(cgroup_path).map_err(|error| {
        ProviderError::new("linux-device-bpf", "CGROUP_OPEN_FAILED", &error.to_string())
    })?;
    let attach = BpfProgAttachAttr {
        target_fd: cgroup.as_raw_fd() as u32,
        attach_bpf_fd: program_fd as u32,
        attach_type: BPF_CGROUP_DEVICE,
        attach_flags: 0,
        replace_bpf_fd: 0,
    };
    let attached = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_ATTACH,
            &attach,
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    unsafe {
        libc::close(program_fd as libc::c_int);
    }
    if attached < 0 {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_BPF_ATTACH_FAILED",
            &std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn attach_device_bpf_filter(
    _cgroup_path: &Path,
    _binding: &DeviceBinding,
) -> Result<(), ProviderError> {
    Err(ProviderError::new(
        "linux-device-bpf",
        "HARD_ENFORCEMENT_UNAVAILABLE",
        "cgroup device BPF requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn device_rule(node: &cy_kernel_api::DeviceNode) -> Result<Option<DeviceRule>, ProviderError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match fs::metadata(&node.path) {
        Ok(metadata) => metadata,
        Err(error) if !node.required => return Ok(None),
        Err(error) => {
            return Err(ProviderError::new(
                "linux-device-bpf",
                "DEVICE_NODE_MISSING",
                &format!("{}: {error}", node.path.display()),
            ))
        }
    };
    let kind = if metadata.file_type().is_char_device() {
        BPF_DEVCG_DEV_CHAR
    } else if metadata.file_type().is_block_device() {
        BPF_DEVCG_DEV_BLOCK
    } else {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_NODE_TYPE_INVALID",
            &node.path.display().to_string(),
        ));
    };
    let major = (metadata.rdev() >> 8) & 0x0fff;
    let minor = (metadata.rdev() & 0xff) | ((metadata.rdev() >> 12) & 0x0fff00);
    if node.major.is_some_and(|expected| expected != major as u32)
        || node.minor.is_some_and(|expected| expected != minor as u32)
    {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_NODE_IDENTITY_CHANGED",
            &node.path.display().to_string(),
        ));
    }
    Ok(Some(DeviceRule {
        kind,
        major: major as u32,
        minor: minor as u32,
    }))
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct DeviceRule {
    kind: u32,
    major: u32,
    minor: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct BpfInsn {
    code: u8,
    dst_src: u8,
    off: i16,
    imm: i32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct BpfProgLoadAttr {
    prog_type: u32,
    insn_cnt: u32,
    insns: u64,
    license: u64,
    log_level: u32,
    log_size: u32,
    log_buf: u64,
    kern_version: u32,
    prog_flags: u32,
    prog_name: [u8; 16],
    prog_ifindex: u32,
    expected_attach_type: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct BpfProgAttachAttr {
    target_fd: u32,
    attach_bpf_fd: u32,
    attach_type: u32,
    attach_flags: u32,
    replace_bpf_fd: u32,
}

#[cfg(target_os = "linux")]
fn build_device_filter_program(rules: &[DeviceRule]) -> Vec<BpfInsn> {
    let mut program = vec![
        insn(BPF_LDX_W_MEM, 4, 1, 0, 0), // ctx.access_type
        insn(BPF_LDX_W_MEM, 2, 1, 4, 0), // ctx.major
        insn(BPF_LDX_W_MEM, 3, 1, 8, 0), // ctx.minor
        insn(BPF_ALU64_AND_K, 4, 0, 0, 0xffff_0000_u32 as i32),
    ];
    let mut skip_indices = Vec::new();
    for rule in rules {
        let start = program.len();
        program.push(insn(BPF_JMP_JEQ_K, 4, 0, 1, rule.kind as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_JMP_JEQ_K, 2, 0, 1, rule.major as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_JMP_JEQ_K, 3, 0, 1, rule.minor as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_ALU64_MOV_K, 0, 0, 0, 1));
        program.push(insn(BPF_JMP_EXIT, 0, 0, 0, 0));
        let next = program.len();
        for index in skip_indices.drain(..) {
            program[index].off = (next - index - 1) as i16;
        }
        debug_assert_eq!(start + 8, next);
    }
    program.push(insn(BPF_ALU64_MOV_K, 0, 0, 0, 0));
    program.push(insn(BPF_JMP_EXIT, 0, 0, 0, 0));
    program
}

#[cfg(target_os = "linux")]
fn insn(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> BpfInsn {
    BpfInsn {
        code,
        dst_src: dst | (src << 4),
        off,
        imm,
    }
}

#[cfg(target_os = "linux")]
const BPF_PROG_LOAD: libc::c_long = 5;
#[cfg(target_os = "linux")]
const BPF_PROG_ATTACH: libc::c_long = 8;
#[cfg(target_os = "linux")]
const BPF_PROG_TYPE_CGROUP_DEVICE: u32 = 15;
#[cfg(target_os = "linux")]
const BPF_CGROUP_DEVICE: u32 = 6;
#[cfg(target_os = "linux")]
const BPF_DEVCG_DEV_BLOCK: u32 = 1 << 16;
#[cfg(target_os = "linux")]
const BPF_DEVCG_DEV_CHAR: u32 = 2 << 16;
#[cfg(target_os = "linux")]
const BPF_LDX_W_MEM: u8 = 0x61;
#[cfg(target_os = "linux")]
const BPF_ALU64_AND_K: u8 = 0x57;
#[cfg(target_os = "linux")]
const BPF_ALU64_MOV_K: u8 = 0xb7;
#[cfg(target_os = "linux")]
const BPF_JMP_JEQ_K: u8 = 0x15;
#[cfg(target_os = "linux")]
const BPF_JMP_A: u8 = 0x05;
#[cfg(target_os = "linux")]
const BPF_JMP_EXIT: u8 = 0x95;

fn is_owned_instance_name(name: &str) -> bool {
    name.starts_with(OWNED_INSTANCE_PREFIX)
        && name.len() > OWNED_INSTANCE_PREFIX.len()
        && safe_cgroup_segment(name)
}

fn safe_cgroup_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn wait_until_empty(path: &Path, timeout: Duration) -> bool {
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

fn fact(name: &str, available: bool, required: bool, detail: &str) -> CapabilityFact {
    CapabilityFact {
        name: name.to_string(),
        available,
        required,
        detail: detail.to_string(),
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn read_key_values(path: impl AsRef<Path>) -> BTreeMap<String, u64> {
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

fn pidfd_available() -> bool {
    #[cfg(target_os = "linux")]
    unsafe {
        let fd = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0);
        if fd < 0 {
            false
        } else {
            libc::close(fd as libc::c_int);
            true
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn proc_start_time(pid: u32) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn config(root: PathBuf) -> CgroupV2Config {
        CgroupV2Config {
            root,
            device_bpf_enabled: false,
        }
    }

    #[test]
    fn preflight_requires_cgroup_v2_and_cgroup_kill() {
        let root = std::env::temp_dir().join(format!("cyrene-cgroup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("cgroup.controllers"), "cpu memory pids").unwrap();
        File::create(root.join("cgroup.kill")).unwrap();
        assert!(CgroupV2Runtime::new(config(root.clone())).preflight().ready);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn owned_cleanup_never_selects_non_instance_children() {
        let root = std::env::temp_dir().join(format!("cyrene-cleanup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("foreign")).unwrap();
        fs::create_dir_all(root.join("instance-stale")).unwrap();
        File::create(root.join("instance-stale/cgroup.procs")).unwrap();
        File::create(root.join("instance-stale/cgroup.kill")).unwrap();
        // A normal directory cannot emulate cgroupfs: its virtual control
        // files remain ordinary files, so removal must fail closed. The key
        // invariant is that the unrelated sibling is never selected.
        let error = CgroupV2Runtime::new(config(root.clone()))
            .cleanup_owned_instances()
            .unwrap_err();
        assert_eq!(error.reason_code, "OWNED_CGROUP_CLEANUP_INCOMPLETE");
        assert!(root.join("foreign").exists());
        assert!(root.join("instance-stale").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn telemetry_uses_kernel_files_without_estimation() {
        let root = std::env::temp_dir().join(format!("cyrene-telemetry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("memory.current"), "42\n").unwrap();
        fs::write(root.join("memory.events.local"), "oom_kill 7\n").unwrap();
        fs::write(
            root.join("cpu.stat"),
            "usage_usec 11\nuser_usec 8\nsystem_usec 3\n",
        )
        .unwrap();
        let telemetry = CgroupV2Runtime::new(config(root.clone())).telemetry(&ProcessHandle {
            pid: 1,
            cgroup_path: root.clone(),
            start_time_ticks: None,
        });
        assert_eq!(telemetry.memory_current_bytes, Some(42));
        assert_eq!(telemetry.cpu_usage_usec, Some(11));
        assert_eq!(telemetry.oom_kill_count, 7);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hard_device_request_fails_closed_when_disabled() {
        let mapper = LinuxDeviceMapper {
            device_bpf_enabled: false,
        };
        let binding = DeviceBinding {
            device_id: "GPU-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Hard,
            adapter_id: "test".to_string(),
            reason_code: "test".to_string(),
        };
        assert_eq!(
            mapper
                .enforce(&binding, EnforcementMode::Hard)
                .unwrap_err()
                .reason_code,
            "HARD_ENFORCEMENT_UNAVAILABLE"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn device_filter_program_is_default_deny_and_has_allow_path() {
        let program = build_device_filter_program(&[DeviceRule {
            kind: BPF_DEVCG_DEV_CHAR,
            major: 195,
            minor: 0,
        }]);
        assert_eq!(program.last().unwrap().code, BPF_JMP_EXIT);
        assert_eq!(program[program.len() - 2].imm, 0);
        assert!(program
            .iter()
            .any(|instruction| instruction.code == BPF_ALU64_MOV_K && instruction.imm == 1));
    }
}
