//! Privileged Linux cgroup v2 execution runtime backend.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{
    fs::{FileTypeExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};

#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

use cy_kernel_api::{
    CgroupLimits, CgroupTelemetry, CleanupReport, DeviceBinding, EnforcementMode,
    EnforcementReport, LaunchPlan, NodeCapabilities, ProcessCondition, ProcessHandle,
    ProcessRuntime, ProviderError, RuntimeProcessEvidence, SandboxBackend, StopRequest,
};

#[cfg(target_os = "linux")]
use crate::sys::open_pidfd;
use crate::{
    bpf::attach_device_bpf_filter,
    config::{fact, is_owned_instance_name, CgroupV2Config, OwnedCgroupCleanupReport},
    sys::{
        pidfd_available, proc_start_time, read_key_values, read_oom_kill_count, read_trimmed,
        wait_until_empty,
    },
};

const CPU_PERIOD_USEC: u64 = 100_000;

/// Linux cgroup v2 execution backend. The `children` map exists solely for
/// bounded reaping; cgroup.kill remains the authority for the whole tree.
#[derive(Debug)]
pub struct CgroupV2Runtime {
    config: CgroupV2Config,
    children: Mutex<BTreeMap<u32, TrackedChild>>,
}

/// A child and its Linux wait handle. The pidfd is opened immediately after
/// spawn, so a later PID reuse cannot make a cleanup path wait for the wrong
/// process. Platforms without pidfd retain the bounded Child fallback.
#[derive(Debug)]
struct TrackedChild {
    child: Child,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
    #[cfg(unix)]
    transport: Option<WorkerTransportControl>,
}

impl TrackedChild {
    fn new(child: Child, #[cfg(unix)] transport: Option<WorkerTransportControl>) -> Self {
        #[cfg(target_os = "linux")]
        let pidfd = open_pidfd(child.id());
        Self {
            child,
            #[cfg(target_os = "linux")]
            pidfd,
            #[cfg(unix)]
            transport,
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct WorkerTransportControl {
    stop: Arc<AtomicBool>,
    socket_path: PathBuf,
    stream: Arc<Mutex<Option<UnixStream>>>,
}

#[cfg(unix)]
impl WorkerTransportControl {
    fn new(socket_path: PathBuf) -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            socket_path,
            stream: Arc::new(Mutex::new(None)),
        }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(stream) = stream.take() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
impl Drop for WorkerTransportControl {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
fn bind_worker_transport(path: &Path) -> Result<UnixListener, ProviderError> {
    let parent = path.parent().ok_or_else(|| {
        ProviderError::new(
            "native-process",
            "WORKER_TRANSPORT_PATH_INVALID",
            "worker transport socket needs a parent directory",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ProviderError::new(
            "native-process",
            "WORKER_TRANSPORT_PARENT_FAILED",
            &error.to_string(),
        )
    })?;
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            ProviderError::new(
                "native-process",
                "WORKER_TRANSPORT_STAT_FAILED",
                &error.to_string(),
            )
        })?;
        if !metadata.file_type().is_socket() {
            return Err(ProviderError::new(
                "native-process",
                "WORKER_TRANSPORT_PATH_INVALID",
                "refusing to replace a non-socket worker transport path",
            ));
        }
        fs::remove_file(path).map_err(|error| {
            ProviderError::new(
                "native-process",
                "WORKER_TRANSPORT_REMOVE_FAILED",
                &error.to_string(),
            )
        })?;
    }
    let listener = UnixListener::bind(path).map_err(|error| {
        ProviderError::new(
            "native-process",
            "WORKER_TRANSPORT_BIND_FAILED",
            &error.to_string(),
        )
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660)).map_err(|error| {
        ProviderError::new(
            "native-process",
            "WORKER_TRANSPORT_PERMISSIONS_FAILED",
            &error.to_string(),
        )
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        ProviderError::new(
            "native-process",
            "WORKER_TRANSPORT_NONBLOCKING_FAILED",
            &error.to_string(),
        )
    })?;
    Ok(listener)
}

#[cfg(unix)]
fn start_worker_transport_bridge(
    listener: UnixListener,
    mut stdin: std::process::ChildStdin,
    mut stdout: std::process::ChildStdout,
    control: WorkerTransportControl,
) {
    thread::spawn(move || {
        while !control.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let Ok(read_stream) = stream.try_clone() else {
                        control.stop();
                        return;
                    };
                    if let Ok(mut current) = control.stream.lock() {
                        *current = stream.try_clone().ok();
                    }
                    thread::spawn(move || {
                        let mut read_stream = read_stream;
                        let _ = std::io::copy(&mut read_stream, &mut stdin);
                    });
                    let mut write_stream = stream;
                    let _ = std::io::copy(&mut stdout, &mut write_stream);
                    control.stop();
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    control.stop();
                    return;
                }
            }
        }
        control.stop();
    });
}

impl CgroupV2Runtime {
    pub fn new(config: CgroupV2Config) -> Self {
        Self {
            config,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// Creates and validates the dedicated root and enables available
    /// controllers on its parent. Restart cleanup is intentionally deferred to
    /// the Kernel's journal-backed recovery pass; sandboxd never guesses that a
    /// direct child belongs to a previous Kernel process.
    pub fn initialize_owned_root(&self) -> Result<OwnedCgroupCleanupReport, ProviderError> {
        self.validate_owned_root()?;
        fs::create_dir_all(&self.config.root).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ROOT_CREATE_FAILED",
                &error.to_string(),
            )
        })?;
        if !self.config.dev_mode {
            self.enable_parent_controllers()?;
        }
        Ok(OwnedCgroupCleanupReport::default())
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

    fn recovery_process_ids(&self, cgroup_path: &Path) -> Result<Vec<u32>, ProviderError> {
        let contents = fs::read_to_string(cgroup_path.join("cgroup.procs")).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "RECOVERY_DISCOVERY_FAILED",
                &error.to_string(),
            )
        })?;
        contents
            .split_whitespace()
            .map(|pid| {
                pid.parse::<u32>().map_err(|_| {
                    ProviderError::new(
                        "linux-cgroup-v2",
                        "RECOVERY_DISCOVERY_INVALID",
                        "cgroup.procs contains an invalid PID",
                    )
                })
            })
            .collect()
    }

    fn discover_recovery_processes_inner(
        &self,
    ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
        let entries = fs::read_dir(&self.config.root).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "RECOVERY_DISCOVERY_FAILED",
                &error.to_string(),
            )
        })?;
        let mut processes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "RECOVERY_DISCOVERY_FAILED",
                    &error.to_string(),
                )
            })?;
            if !entry
                .file_type()
                .map_err(|error| {
                    ProviderError::new(
                        "linux-cgroup-v2",
                        "RECOVERY_DISCOVERY_FAILED",
                        &error.to_string(),
                    )
                })?
                .is_dir()
            {
                continue;
            }
            let cgroup_name = entry.file_name().into_string().map_err(|_| {
                ProviderError::new(
                    "linux-cgroup-v2",
                    "RECOVERY_DISCOVERY_INVALID",
                    "sandbox cgroup name is not UTF-8",
                )
            })?;
            for pid in self.recovery_process_ids(&entry.path())? {
                let start_time_ticks = proc_start_time(pid).ok_or_else(|| {
                    ProviderError::new(
                        "linux-cgroup-v2",
                        "RECOVERY_EVIDENCE_UNAVAILABLE",
                        "live sandbox process has no readable start time",
                    )
                })?;
                processes.push(RuntimeProcessEvidence {
                    cgroup_name: cgroup_name.clone(),
                    pid,
                    start_time_ticks,
                });
            }
        }
        Ok(processes)
    }

    fn recover_stale_process_inner(
        &self,
        evidence: &RuntimeProcessEvidence,
    ) -> Result<CleanupReport, ProviderError> {
        let cgroup_path = self.cgroup_path(&evidence.cgroup_name)?;
        let pids = self.recovery_process_ids(&cgroup_path)?;
        if !pids.contains(&evidence.pid)
            || proc_start_time(evidence.pid) != Some(evidence.start_time_ticks)
        {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "RECOVERY_EVIDENCE_MISMATCH",
                "persisted process evidence no longer matches the sandbox runtime",
            ));
        }
        let before_oom = read_oom_kill_count(&cgroup_path);
        self.kill_cgroup(&cgroup_path)?;
        let exit_code = self
            .children
            .lock()
            .map(|children| children.contains_key(&evidence.pid))
            .unwrap_or(false)
            .then(|| self.wait_child(evidence.pid, Duration::from_secs(10)))
            .flatten();
        let complete = wait_until_empty(&cgroup_path, Duration::from_secs(10));
        let oom_killed = read_oom_kill_count(&cgroup_path) > before_oom;
        if complete {
            let _ = fs::remove_dir(&cgroup_path);
        }
        Ok(CleanupReport {
            complete,
            exit_code,
            oom_killed,
            conditions: if complete {
                Vec::new()
            } else {
                vec![ProcessCondition {
                    reason_code: "CGROUP_NOT_EMPTY".to_string(),
                    summary: "stale sandbox cgroup remained after bounded recovery cleanup"
                        .to_string(),
                }]
            },
            reason_code: if complete {
                "RECOVERY_CLEANUP_COMPLETE".to_string()
            } else {
                "RECOVERY_CLEANUP_INCOMPLETE".to_string()
            },
        })
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
        let required = !self.config.dev_mode;
        let facts = vec![
            fact("cgroup-v2", cgroup_v2, required, "owned cgroup.controllers"),
            fact("cgroup-kill", cgroup_kill, required, "owned cgroup.kill"),
            fact(
                "memory-controller",
                has_controller("memory"),
                required,
                "memory",
            ),
            fact("pids-controller", has_controller("pids"), required, "pids"),
            fact("cpu-controller", has_controller("cpu"), required, "cpu"),
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
            fact(
                "dev-mode",
                self.config.dev_mode,
                false,
                "development mode bypassing hardware cgroup enforcement",
            ),
        ];
        let ready = self.config.dev_mode
            || facts
                .iter()
                .filter(|fact| fact.required)
                .all(|fact| fact.available);
        NodeCapabilities {
            ready,
            facts,
            enforcement: vec![EnforcementReport {
                resource_kind: "process-tree".to_string(),
                mode: if self.config.dev_mode {
                    EnforcementMode::Unenforced
                } else if ready {
                    EnforcementMode::Hard
                } else {
                    EnforcementMode::Unenforced
                },
                adapter_id: "linux-cgroup-v2".to_string(),
                reason_code: if self.config.dev_mode {
                    "DEV_MODE_UNENFORCED".to_string()
                } else if ready {
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

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn cleanup_owned_instances(
        &self,
    ) -> Result<OwnedCgroupCleanupReport, ProviderError> {
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

    pub(crate) fn create_instance_cgroup(&self, name: &str) -> Result<PathBuf, ProviderError> {
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
        if self.config.dev_mode && !cgroup_path.join("cgroup.procs").exists() {
            return Ok(());
        }
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
        if self.config.dev_mode && !cgroup_path.join("cgroup.procs").exists() {
            return Ok(());
        }
        fs::write(cgroup_path.join("cgroup.procs"), pid.to_string()).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ATTACH_FAILED",
                &error.to_string(),
            )
        })
    }

    fn kill_cgroup(&self, cgroup_path: &Path) -> Result<(), ProviderError> {
        if self.config.dev_mode && !cgroup_path.join("cgroup.kill").exists() {
            return Ok(());
        }
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
        #[cfg(target_os = "linux")]
        if let Some(result) = self.wait_child_with_pidfd(pid, timeout) {
            return result;
        }

        let deadline = Instant::now() + timeout;
        loop {
            let result = self.children.lock().ok().and_then(|mut children| {
                children
                    .get_mut(&pid)
                    .and_then(|child| child.child.try_wait().ok())
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

    /// Returns `Some` only when a tracked child had a pidfd. A pidfd readiness
    /// notification is then followed by `Child::wait` to reap the zombie;
    /// timeout leaves the child tracked for the cgroup.kill phase or a later
    /// cleanup attempt.
    #[cfg(target_os = "linux")]
    fn wait_child_with_pidfd(&self, pid: u32, timeout: Duration) -> Option<Option<i32>> {
        let mut tracked = self
            .children
            .lock()
            .ok()
            .and_then(|mut children| children.remove(&pid))?;
        let Some(pidfd) = tracked.pidfd.as_ref() else {
            if let Ok(mut children) = self.children.lock() {
                children.insert(pid, tracked);
            }
            return None;
        };
        let mut descriptor = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(pidfd),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: descriptor points to the owned pidfd, and poll only observes
        // readiness. The adapter owns this file descriptor for this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if ready > 0 {
            return Some(tracked.child.wait().ok().and_then(|status| status.code()));
        }
        if let Ok(mut children) = self.children.lock() {
            children.insert(pid, tracked);
        }
        Some(None)
    }

    fn terminate_pid(&self, pid: u32) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(windows)]
        if let Ok(mut children) = self.children.lock() {
            if let Some(child) = children.get_mut(&pid) {
                let _ = child.child.kill();
            }
        }
    }

    fn kill_pid(&self, pid: u32) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(windows)]
        if let Ok(mut children) = self.children.lock() {
            if let Some(child) = children.get_mut(&pid) {
                let _ = child.child.kill();
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
        #[cfg(unix)]
        let transport_listener = if let Some(path) = plan.transport_socket.as_ref() {
            if !path.is_absolute()
                || !path.starts_with(&self.config.transport_root)
                || path.parent() != Some(self.config.transport_root.as_path())
            {
                let _ = self.kill_cgroup(&cgroup_path);
                let _ = fs::remove_dir(&cgroup_path);
                return Err(ProviderError::new(
                    "native-process",
                    "WORKER_TRANSPORT_PATH_INVALID",
                    "worker transport socket must be a direct child of sandboxd transport_root",
                ));
            }
            match bind_worker_transport(path) {
                Ok(listener) => Some(listener),
                Err(error) => {
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let mut command = Command::new(&plan.executable);
        command
            .args(&plan.args)
            .envs(environment)
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        if transport_listener.is_some() {
            command.stdin(Stdio::piped()).stdout(Stdio::piped());
        } else {
            command.stdin(Stdio::null()).stdout(Stdio::null());
        }
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = plan.transport_socket.as_ref() {
                    let _ = fs::remove_file(path);
                }
                let _ = self.kill_cgroup(&cgroup_path);
                let _ = fs::remove_dir(&cgroup_path);
                return Err(ProviderError::new(
                    "native-process",
                    "SPAWN_FAILED",
                    &error.to_string(),
                ));
            }
        };
        #[cfg(unix)]
        let transport = if let Some(listener) = transport_listener {
            let stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_file(
                        plan.transport_socket
                            .as_ref()
                            .expect("transport listener has a path"),
                    );
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdin was not piped",
                    ));
                }
            };
            let stdout = match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_file(
                        plan.transport_socket
                            .as_ref()
                            .expect("transport listener has a path"),
                    );
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdout was not piped",
                    ));
                }
            };
            let path = plan
                .transport_socket
                .as_ref()
                .expect("transport listener has a path")
                .clone();
            let control = WorkerTransportControl::new(path);
            start_worker_transport_bridge(listener, stdin, stdout, control.clone());
            Some(control)
        } else {
            None
        };
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
            .insert(pid, TrackedChild::new(child, transport));
        Ok(ProcessHandle {
            pid,
            cgroup_path,
            start_time_ticks: proc_start_time(pid),
            transport_socket: plan.transport_socket.clone(),
        })
    }

    fn stop(
        &self,
        handle: &ProcessHandle,
        request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError> {
        #[cfg(unix)]
        if let Ok(children) = self.children.lock() {
            if let Some(child) = children.get(&handle.pid) {
                if let Some(transport) = child.transport.as_ref() {
                    transport.stop();
                }
            }
        }
        let before_oom = read_oom_kill_count(&handle.cgroup_path);
        let mut exit_code = None;
        if request.immediate {
            if self.config.dev_mode {
                self.kill_pid(handle.pid);
                exit_code = self.wait_child(
                    handle.pid,
                    request.grace_period.min(Duration::from_millis(500)),
                );
            } else if !self.cgroup_is_empty(&handle.cgroup_path) {
                self.kill_cgroup(&handle.cgroup_path)?;
                exit_code = self.wait_child(handle.pid, request.grace_period);
                wait_until_empty(
                    &handle.cgroup_path,
                    request.grace_period.max(Duration::from_millis(500)),
                );
            }
        } else {
            self.terminate_pid(handle.pid);
            exit_code = self.wait_child(handle.pid, request.grace_period);
            if exit_code.is_none() {
                if self.config.dev_mode {
                    self.kill_pid(handle.pid);
                    exit_code = self.wait_child(handle.pid, Duration::from_millis(500));
                } else if !self.cgroup_is_empty(&handle.cgroup_path) {
                    self.kill_cgroup(&handle.cgroup_path)?;
                    exit_code = self.wait_child(handle.pid, request.grace_period);
                    wait_until_empty(
                        &handle.cgroup_path,
                        request.grace_period.max(Duration::from_millis(500)),
                    );
                }
            }
        }
        let complete = (self.config.dev_mode || self.cgroup_is_empty(&handle.cgroup_path))
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
            if self.config.dev_mode {
                let _ = fs::remove_dir_all(&handle.cgroup_path);
            } else {
                let _ = fs::remove_dir(&handle.cgroup_path);
            }
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

    fn discover_recovery_processes(&self) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
        self.discover_recovery_processes_inner()
    }

    fn recover_stale_process(
        &self,
        evidence: &RuntimeProcessEvidence,
    ) -> Result<CleanupReport, ProviderError> {
        self.recover_stale_process_inner(evidence)
    }
}

#[cfg(all(test, unix))]
mod transport_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn worker_stdio_bridge_round_trips_bytes_without_protocol_knowledge() {
        let socket =
            std::env::temp_dir().join(format!("cyrene-bridge-{}.sock", std::process::id()));
        let _ = fs::remove_file(&socket);
        let listener = bind_worker_transport(&socket).expect("bind worker socket");
        let mut child = Command::new("sh")
            .args(["-c", "cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn byte worker");
        let stdin = child.stdin.take().expect("worker stdin");
        let stdout = child.stdout.take().expect("worker stdout");
        let control = WorkerTransportControl::new(socket.clone());
        start_worker_transport_bridge(listener, stdin, stdout, control.clone());

        let mut stream = None;
        for _ in 0..50 {
            match UnixStream::connect(&socket) {
                Ok(value) => {
                    stream = Some(value);
                    break;
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("worker bridge should accept a client");
        stream.write_all(b"hello\n").expect("write worker input");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("close worker input");
        let mut response = [0_u8; 5];
        stream
            .read_exact(&mut response)
            .expect("read worker output");
        assert_eq!(&response, b"hello");
        let status = child.wait().expect("wait byte worker");
        assert!(status.success());
        control.stop();
        assert!(!socket.exists());
    }
}
