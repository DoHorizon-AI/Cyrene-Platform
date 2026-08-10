//! CYRENE Linux Cgroup v2 原生沙箱与进程隔离运行时 (Sandbox & Cgroup v2 Execution Engine).
//!
//! 【安全沙箱核心设计与不变性约束 (Invariants)】
//! 本模块基于现代 Linux `cgroup v2` 机制实现了原生进程沙箱。为了防止僵尸进程驻留、显存/内存泄漏或资源逃逸，
//! 运行时采取极其严格的防御性设计原则：
//! 1. **就绪门禁 (Preflight Check)**：严格依赖宿主机开启 cgroup v2 统一层级树以及 `cgroup.kill` 特性；
//! 2. **父子进程联动死锁自杀 (PDEATHSIG)**：Linux 下子进程启动前通过 `libc::prctl(PR_SET_PDEATHSIG, SIGKILL)` 声明：一旦内核主控进程崩溃，内核自动向子进程下发 SIGKILL，彻底防止孤儿进程无控消耗 GPU；
//! 3. **原子整树强杀 (cgroup.kill)**：终止进程时向 `cgroup.kill` 写入 1，由 Linux 内核在内核态原子广播信号强杀整个进程子树（包含 fork/vfork 的深层后代）；
//! 4. **终态清理强检验 (Non-Empty Gating)**：只要 `cgroup.procs` 中还存在任何残留 PID，清理操作绝不上报成功，并强制保留资源租约且隔离硬件（Quarantine）；
//! 5. **OOM 精准归因**：通过比对 `memory.events.local` 中 `oom_kill` 计数器增量，准确区分常规崩溃与内存超限 OOM 强杀。

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
    CapabilityFact, CleanupReport, DeviceBinding, DeviceMapper, EnforcementMode, EnforcementReport,
    LaunchPlan, NodeCapabilities, ProcessCondition, ProcessHandle, ProcessRuntime, ProviderError,
    SandboxBackend, StopRequest,
};

/// Cgroup v2 配置参数
#[derive(Debug, Clone)]
pub struct CgroupV2Config {
    /// Cgroup v2 虚拟文件系统挂载根目录（默认为 `/sys/fs/cgroup`）
    pub root: PathBuf,
    /// 宿主机内核是否支持 cgroup 设备 eBPF 过滤程序
    pub device_bpf_available: bool,
}

impl CgroupV2Config {
    /// 获取宿主机标准默认配置
    pub fn host_default() -> Self {
        Self {
            root: PathBuf::from("/sys/fs/cgroup"),
            device_bpf_available: false,
        }
    }
}

/// 基于 Linux cgroup v2 的进程沙箱执行引擎
#[derive(Debug)]
pub struct CgroupV2Runtime {
    /// 配置项
    config: CgroupV2Config,
    /// 管理的活跃子进程表 (`pid -> std::process::Child`)
    children: Mutex<BTreeMap<u32, Child>>,
}

impl CgroupV2Runtime {
    /// 创建沙箱运行时实例
    pub fn new(config: CgroupV2Config) -> Self {
        Self {
            config,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// 校验沙箱组名合法性并计算其对应的 cgroup 目录绝对路径
    pub fn cgroup_path(&self, name: &str) -> Result<PathBuf, ProviderError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "INVALID_CGROUP_NAME",
                name,
            ));
        }
        Ok(self.config.root.join(name))
    }

    /// 节点环境预检：扫描并评估 cgroup 控制器、`cgroup.kill` 与 `pidfd` 等内核特性
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
        let pidfd = pidfd_available();
        let facts = vec![
            fact("cgroup-v2", cgroup_v2, true, "cgroup.controllers"),
            fact("cgroup-kill", cgroup_kill, true, "cgroup.kill"),
            fact(
                "memory-controller",
                has_controller("memory"),
                true,
                "memory",
            ),
            fact("pids-controller", has_controller("pids"), true, "pids"),
            fact("cpu-controller", has_controller("cpu"), true, "cpu"),
            fact("pidfd", pidfd, false, "pidfd_open feature probe"),
            fact(
                "device-bpf",
                self.config.device_bpf_available,
                false,
                "configured cgroup device BPF enforcement",
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

    /// 为新进程实例创建专属的 cgroup 子目录
    pub fn create_instance_cgroup(&self, name: &str) -> Result<PathBuf, ProviderError> {
        let path = self.cgroup_path(name)?;
        fs::create_dir_all(&path).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_CREATE_FAILED",
                &error.to_string(),
            )
        })?;
        Ok(path)
    }

    /// 将特定 PID 附加写入该 cgroup 的 `cgroup.procs` 文件
    fn attach(&self, cgroup_path: &Path, pid: u32) -> Result<(), ProviderError> {
        fs::write(cgroup_path.join("cgroup.procs"), pid.to_string()).map_err(|error| {
            ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_ATTACH_FAILED",
                &error.to_string(),
            )
        })
    }

    /// 向 `cgroup.kill` 写入 "1"，触发内核原子强杀该 cgroup 内的所有进程
    fn kill_cgroup(&self, cgroup_path: &Path) -> Result<(), ProviderError> {
        fs::write(cgroup_path.join("cgroup.kill"), "1").map_err(|error| {
            ProviderError::new("linux-cgroup-v2", "CGROUP_KILL_FAILED", &error.to_string())
        })
    }

    /// 检查该 cgroup 是否已经彻底清空（`cgroup.procs` 为空）
    fn cgroup_is_empty(&self, cgroup_path: &Path) -> bool {
        read_trimmed(cgroup_path.join("cgroup.procs"))
            .map(|value| value.is_empty())
            .unwrap_or(true)
    }

    /// 在限定时间内轮询等待子进程退出并获取退出码
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

    /// 向指定 PID 发送 SIGTERM（Linux）或强制杀死（Windows）
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
        let environment = binding.merge_environment(&plan.environment)?;
        let mut command = Command::new(&plan.executable);
        command.args(&plan.args).envs(environment);
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                // 守护绑定：主进程退出时自动下发 SIGKILL 强杀子进程
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
        // 1. 若非立即强杀，先尝试优雅发送 SIGTERM 并等待宽限期
        if !request.immediate {
            self.terminate_pid(handle.pid);
            exit_code = self.wait_child(handle.pid, request.grace_period);
        }
        // 2. 若 cgroup 仍不为空，调用 cgroup.kill 强杀整个子树
        if !self.cgroup_is_empty(&handle.cgroup_path) {
            self.kill_cgroup(&handle.cgroup_path)?;
            if exit_code.is_none() {
                exit_code = self.wait_child(handle.pid, request.grace_period);
            }
        }
        // 3. 严格校验：只有 cgroup 彻底清空且本地子进程表已移除，才认定清理完成
        let complete = self.cgroup_is_empty(&handle.cgroup_path)
            && !self
                .children
                .lock()
                .map(|children| children.contains_key(&handle.pid))
                .unwrap_or(true);
        let after_oom = read_oom_kill_count(&handle.cgroup_path);
        let oom_killed = after_oom > before_oom;
        let mut conditions = Vec::new();
        if !complete {
            conditions.push(ProcessCondition {
                reason_code: "REAP_TIMEOUT".to_string(),
                summary: "process or cgroup remained after bounded cleanup".to_string(),
            });
            conditions.push(ProcessCondition {
                reason_code: "CGROUP_NOT_EMPTY".to_string(),
                summary: "resource lease must remain held and device must be quarantined"
                    .to_string(),
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
}

impl SandboxBackend for CgroupV2Runtime {
    fn backend_id(&self) -> &str {
        "native-cgroup-v2"
    }
}

/// Linux 设备隔离策略映射器
#[derive(Debug, Clone, Copy)]
pub struct LinuxDeviceMapper {
    pub device_bpf_available: bool,
}

impl DeviceMapper for LinuxDeviceMapper {
    fn enforce(
        &self,
        binding: &DeviceBinding,
        requested: EnforcementMode,
    ) -> Result<EnforcementReport, ProviderError> {
        if requested == EnforcementMode::Hard && !self.device_bpf_available {
            return Err(ProviderError::new(
                "linux-device-bpf",
                "HARD_ENFORCEMENT_UNAVAILABLE",
                "cgroup device BPF was not detected; visibility cannot be promoted to HARD",
            ));
        }
        let mode = if self.device_bpf_available {
            requested
        } else {
            EnforcementMode::VisibilityOnly
        };
        Ok(EnforcementReport {
            resource_kind: "accelerator".to_string(),
            mode,
            adapter_id: binding.adapter_id.clone(),
            reason_code: if mode == EnforcementMode::Hard {
                "CGROUP_DEVICE_BPF".to_string()
            } else {
                "VISIBILITY_ONLY".to_string()
            },
        })
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

pub fn read_oom_kill_count(cgroup_path: &Path) -> u64 {
    let Some(events) = read_trimmed(cgroup_path.join("memory.events.local")) else {
        return 0;
    };
    events
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(name, _)| *name == "oom_kill")
        .and_then(|(_, value)| value.parse::<u64>().ok())
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
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.split_whitespace().nth(21)?.parse().ok()
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

    #[test]
    fn preflight_requires_cgroup_v2_and_cgroup_kill() {
        let root = std::env::temp_dir().join(format!("cyrene-cgroup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("cgroup.controllers"), "cpu memory pids").unwrap();
        File::create(root.join("cgroup.kill")).unwrap();
        let runtime = CgroupV2Runtime::new(CgroupV2Config {
            root: root.clone(),
            device_bpf_available: false,
        });
        assert!(runtime.preflight().ready);
        assert!(
            !runtime
                .preflight()
                .facts
                .iter()
                .find(|fact| fact.name == "device-bpf")
                .unwrap()
                .available
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oom_diagnosis_uses_memory_events_local() {
        let root = std::env::temp_dir().join(format!("cyrene-oom-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("memory.events.local"),
            "low 0\noom 1\noom_kill 7\n",
        )
        .unwrap();
        assert_eq!(read_oom_kill_count(&root), 7);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hard_device_request_fails_closed_without_bpf() {
        let mapper = LinuxDeviceMapper {
            device_bpf_available: false,
        };
        let binding = DeviceBinding {
            device_id: "GPU-0".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::VisibilityOnly,
            adapter_id: "test-hardware-adapter".to_string(),
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
}
