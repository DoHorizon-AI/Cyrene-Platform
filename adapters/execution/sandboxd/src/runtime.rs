// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/runtime.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Privileged Linux cgroup v2 execution runtime backend.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(any(not(target_os = "linux"), test))]
use std::process::Child;

#[cfg(not(target_os = "linux"))]
use std::process::{Command, Stdio};

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
use crate::sys::{open_pidfd, spawn_gated_process, try_wait_pid, wait_pid};
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
/// 中文：Linux cgroup v2 执行后端。`children` 映射仅用于有界回收；整个进程树的权威终止方式仍是 cgroup.kill。
#[derive(Debug)]
pub struct CgroupV2Runtime {
    config: CgroupV2Config,
    children: Mutex<BTreeMap<u32, TrackedChild>>,
}

/// A child and its Linux wait handle. The pidfd is opened immediately after
/// spawn, so a later PID reuse cannot make a cleanup path wait for the wrong
/// process. Platforms without pidfd retain the bounded Child fallback.
/// 中文：子进程及其 Linux 等待句柄。派生进程后会立即打开 pidfd，因此即使之后发生 PID 重用，清理路径也不会等待到错误进程。没有 pidfd 的平台会保留有界的 Child 回退方式。
#[derive(Debug)]
struct TrackedChild {
    process: TrackedProcess,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
    #[cfg(unix)]
    transport: Option<WorkerTransportControl>,
}

#[derive(Debug)]
enum TrackedProcess {
    #[cfg(any(not(target_os = "linux"), test))]
    Command(Child),
    #[cfg(target_os = "linux")]
    Forked,
}

impl TrackedChild {
    #[cfg(any(not(target_os = "linux"), test))]
    fn new(child: Child, #[cfg(unix)] transport: Option<WorkerTransportControl>) -> Self {
        #[cfg(target_os = "linux")]
        let pidfd = open_pidfd(child.id());
        Self {
            process: TrackedProcess::Command(child),
            #[cfg(target_os = "linux")]
            pidfd,
            #[cfg(unix)]
            transport,
        }
    }

    #[cfg(target_os = "linux")]
    fn new_forked(pid: u32, transport: Option<WorkerTransportControl>) -> Self {
        Self {
            process: TrackedProcess::Forked,
            pidfd: open_pidfd(pid),
            transport,
        }
    }

    #[cfg(target_os = "linux")]
    fn try_wait(&mut self, pid: u32) -> Option<Option<i32>> {
        match &mut self.process {
            #[cfg(test)]
            TrackedProcess::Command(child) => {
                child.try_wait().ok().flatten().map(|status| status.code())
            }
            TrackedProcess::Forked => try_wait_pid(pid),
        }
    }

    #[cfg(target_os = "linux")]
    fn wait(&mut self, pid: u32) -> Option<i32> {
        match &mut self.process {
            #[cfg(test)]
            TrackedProcess::Command(child) => child.wait().ok().and_then(|status| status.code()),
            TrackedProcess::Forked => wait_pid(pid),
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
    mut stdin: impl std::io::Write + Send + 'static,
    mut stdout: impl std::io::Read + Send + 'static,
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
    /// 中文：创建并校验专用根目录，并在其父级启用可用控制器。重启清理会有意延后到 Kernel 基于日志的恢复阶段；sandboxd 不会猜测某个直接子进程是否属于此前的 Kernel 进程。
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
    /// 中文：校验直接实例组名称，并解析其在受管理根目录下的位置。
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
            if let Err(err) = fs::remove_dir(&cgroup_path) {
                tracing::warn!(
                    event.name = "platform.sandbox.cleanup_warning",
                    error.code = "PLATFORM.SANDBOX.CGROUP_INIT_FAILED",
                    cgroup_path = %cgroup_path.display(),
                    error = %err,
                    message = "Failed to remove cgroup directory during recovery cleanup",
                );
            }
            tracing::info!(
                event.name = "platform.sandbox.terminated",
                pid = evidence.pid,
                cgroup_path = %cgroup_path.display(),
                exit_code = ?exit_code,
                oom_killed = oom_killed,
                message = "Stale sandbox process terminated and cgroup cleaned up during recovery",
            );
        } else {
            tracing::error!(
                event.name = "platform.sandbox.terminated",
                error.code = "PLATFORM.SANDBOX.KILL_FAILED",
                pid = evidence.pid,
                cgroup_path = %cgroup_path.display(),
                exit_code = ?exit_code,
                oom_killed = oom_killed,
                message = "Stale sandbox process or cgroup cleanup unconfirmed during recovery",
            );
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
    /// 中文：读取 supervisor 和遥测导出器使用的 cgroup v2 物理计数器。缺失的可选文件保持为 `None`；不会用估算值替代内核计数器。
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
            fact(
                "platform-linux",
                cfg!(target_os = "linux"),
                !self.config.dev_mode,
                "native cgroup v2 sandbox is a Linux system Adapter backend",
            ),
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
            // 中文：单元测试使用合成的 cgroup 文件系统。真实内核根目录始终包含此文件，因此生产环境仍会由 launch/preflight 检查把关。
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
            tracing::warn!(
                event.name = "platform.sandbox.kill_warning",
                error.code = "PLATFORM.SANDBOX.KILL_FAILED",
                cgroup_path = %cgroup_path.display(),
                error = %error,
                message = "Failed to write 1 to cgroup.kill",
            );
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
                children.get_mut(&pid).and_then(|child| {
                    #[cfg(target_os = "linux")]
                    {
                        child.try_wait(pid)
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        child
                            .process
                            .try_wait()
                            .ok()
                            .flatten()
                            .map(|status| status.code())
                    }
                })
            });
            if let Some(status) = result {
                if let Ok(mut children) = self.children.lock() {
                    children.remove(&pid);
                }
                return status;
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
    /// 中文：只有受跟踪子进程拥有 pidfd 时才返回 `Some`。收到 pidfd 就绪通知后，还会调用 `Child::wait` 回收僵尸进程；超时会让子进程继续处于受跟踪状态，留待 cgroup.kill 阶段或后续清理尝试处理。
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
        // 中文：安全性说明：该描述符指向已拥有的 pidfd，poll 只观察就绪状态。本次调用期间，此文件描述符由适配器持有。
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if ready > 0 {
            return Some(tracked.wait(pid));
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
                if let TrackedProcess::Command(process) = &mut child.process {
                    let _ = process.kill();
                }
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
                if let TrackedProcess::Command(process) = &mut child.process {
                    let _ = process.kill();
                }
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
        #[cfg(target_os = "linux")]
        {
            let gated = match spawn_gated_process(plan, &environment, transport_listener.is_some())
            {
                Ok(gated) => gated,
                Err(error) => {
                    if let Some(path) = plan.transport_socket.as_ref() {
                        let _ = fs::remove_file(path);
                    }
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(error);
                }
            };
            let pid = gated.pid();
            if let Err(error) = self.attach(&cgroup_path, pid) {
                gated.abort();
                let _ = self.kill_cgroup(&cgroup_path);
                let _ = fs::remove_dir(&cgroup_path);
                return Err(error);
            }
            let forked = match gated.release() {
                Ok(process) => process,
                Err(error) => {
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(error);
                }
            };
            #[cfg(unix)]
            let transport = if let Some(listener) = transport_listener {
                let Some(stdin) = forked.stdin else {
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdin was not created",
                    ));
                };
                let Some(stdout) = forked.stdout else {
                    let _ = self.kill_cgroup(&cgroup_path);
                    let _ = fs::remove_dir(&cgroup_path);
                    return Err(ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdout was not created",
                    ));
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
            let tracked_child = TrackedChild::new_forked(forked.pid, transport);
            self.children
                .lock()
                .map_err(|_| ProviderError::new("native-process", "LOCK_POISONED", "child table"))?
                .insert(pid, tracked_child);
            Ok(ProcessHandle {
                pid,
                cgroup_path,
                start_time_ticks: proc_start_time(pid),
                transport_socket: plan.transport_socket.clone(),
            })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let mut command = Command::new(&plan.executable);
            command
                .args(&plan.args)
                .envs(environment)
                .stderr(Stdio::inherit());
            if let Some(dir) = &plan.working_dir {
                command.current_dir(dir);
            }
            #[cfg(unix)]
            if transport_listener.is_some() {
                command.stdin(Stdio::piped()).stdout(Stdio::piped());
            } else {
                command.stdin(Stdio::null()).stdout(Stdio::null());
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
                let stdin = child.stdin.take().ok_or_else(|| {
                    ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdin was not piped",
                    )
                })?;
                let stdout = child.stdout.take().ok_or_else(|| {
                    ProviderError::new(
                        "native-process",
                        "WORKER_TRANSPORT_PIPE_FAILED",
                        "worker stdout was not piped",
                    )
                })?;
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
                let _ = child.kill();
                let _ = child.wait();
                let _ = self.kill_cgroup(&cgroup_path);
                let _ = fs::remove_dir(&cgroup_path);
                return Err(error);
            }
            #[cfg(unix)]
            let tracked_child = TrackedChild::new(child, transport);
            #[cfg(not(unix))]
            let tracked_child = TrackedChild::new(child);
            self.children
                .lock()
                .map_err(|_| ProviderError::new("native-process", "LOCK_POISONED", "child table"))?
                .insert(pid, tracked_child);
            Ok(ProcessHandle {
                pid,
                cgroup_path,
                start_time_ticks: proc_start_time(pid),
                transport_socket: plan.transport_socket.clone(),
            })
        }
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
        let mut exit_code;
        if request.immediate {
            if self.config.dev_mode {
                self.kill_pid(handle.pid);
                exit_code = self.wait_child(
                    handle.pid,
                    request.grace_period.min(Duration::from_millis(500)),
                );
            } else {
                if !self.cgroup_is_empty(&handle.cgroup_path) {
                    self.kill_cgroup(&handle.cgroup_path)?;
                }
                // An exited child leaves an empty cgroup but still needs reaping.
                // 子进程退出后 cgroup 可能已空,仍须回收子进程才可释放 Lease。
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
            tracing::error!(
                event.name = "platform.sandbox.terminated",
                error.code = "PLATFORM.SANDBOX.KILL_FAILED",
                pid = handle.pid,
                cgroup_path = %handle.cgroup_path.display(),
                exit_code = ?exit_code,
                oom_killed = oom_killed,
                message = "Sandbox process or cgroup cleanup unconfirmed; quarantine maintained",
            );
        } else {
            tracing::info!(
                event.name = "platform.sandbox.terminated",
                pid = handle.pid,
                cgroup_path = %handle.cgroup_path.display(),
                exit_code = ?exit_code,
                oom_killed = oom_killed,
                message = "Sandbox process terminated and cgroup cleaned up successfully",
            );
        }
        if oom_killed {
            conditions.push(ProcessCondition {
                reason_code: "OOM_KILLED".to_string(),
                summary: "memory.events.local oom_kill counter increased".to_string(),
            });
        }
        if complete {
            let res = if self.config.dev_mode {
                fs::remove_dir_all(&handle.cgroup_path)
            } else {
                fs::remove_dir(&handle.cgroup_path)
            };
            if let Err(err) = res {
                tracing::warn!(
                    event.name = "platform.sandbox.cleanup_warning",
                    error.code = "PLATFORM.SANDBOX.CGROUP_INIT_FAILED",
                    cgroup_path = %handle.cgroup_path.display(),
                    error = %err,
                    message = "Failed to remove cgroup directory during completed cleanup",
                );
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
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
    };

    #[test]
    #[cfg(target_os = "linux")]
    fn immediate_cleanup_reaps_an_exited_child_when_cgroup_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let group = root.path().join("instance-exited");
        fs::create_dir(&group).unwrap();
        fs::write(group.join("cgroup.procs"), "").unwrap();
        let runtime = CgroupV2Runtime::new(CgroupV2Config {
            root: root.path().to_path_buf(),
            transport_root: root.path().join("transport"),
            device_bpf_enabled: false,
            dev_mode: false,
        });
        let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        runtime
            .children
            .lock()
            .unwrap()
            .insert(pid, TrackedChild::new(child, None));
        let handle = ProcessHandle {
            pid,
            cgroup_path: group,
            start_time_ticks: proc_start_time(pid),
            transport_socket: None,
        };
        let report = runtime
            .stop(
                &handle,
                &StopRequest {
                    grace_period: Duration::from_secs(1),
                    immediate: true,
                },
            )
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.exit_code, Some(0));
        assert!(!runtime.children.lock().unwrap().contains_key(&pid));
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn gated_process_does_not_execute_before_release() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("released");
        let plan = LaunchPlan {
            instance_name: "gated-worker".to_string(),
            executable: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_string(),
                format!("printf ready > {}", marker.display()),
            ],
            environment: BTreeMap::new(),
            cgroup_name: "instance-gated".to_string(),
            limits: CgroupLimits::default(),
            working_dir: None,
            transport_socket: None,
        };
        let gated = spawn_gated_process(&plan, &BTreeMap::new(), false).unwrap();
        assert!(!marker.exists(), "user code must remain behind the gate");
        let process = gated.release().unwrap();
        assert_eq!(wait_pid(process.pid), Some(0));
        assert_eq!(fs::read_to_string(marker).unwrap(), "ready");
    }

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
