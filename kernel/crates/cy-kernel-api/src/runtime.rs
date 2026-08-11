//! 进程运行时生命周期管理与遥测模型.

use std::{path::PathBuf, time::Duration};

/// 由 cgroup v2 直接读取的真实物理消耗量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupTelemetry {
    /// `memory.current`，单位为字节。
    pub memory_current_bytes: Option<u64>,
    /// `memory.peak`，单位为字节；旧内核不可用时为 `None`。
    pub memory_peak_bytes: Option<u64>,
    /// `cpu.stat` 的总使用时间，单位为微秒。
    pub cpu_usage_usec: Option<u64>,
    /// `cpu.stat` 的用户态使用时间，单位为微秒。
    pub cpu_user_usec: Option<u64>,
    /// `cpu.stat` 的内核态使用时间，单位为微秒。
    pub cpu_system_usec: Option<u64>,
    /// `memory.events.local` 的 `oom_kill` 计数。
    pub oom_kill_count: u64,
}

/// 已启动沙箱进程的句柄引用 (Process Handle)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessHandle {
    /// 主进程操作系统 PID
    pub pid: u32,
    /// 对应的 Linux cgroup v2 路径
    pub cgroup_path: PathBuf,
    /// 进程启动时间滴答数（用于校验 PID 复用）
    pub start_time_ticks: Option<u64>,
}

/// 进程运行状态与异常条件
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCondition {
    /// 状态原因代码
    pub reason_code: String,
    /// 状态描述摘要
    pub summary: String,
}

/// 进程退出与资源清理报告 (Cleanup Report)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    /// 清理操作是否彻底完成（进程树已全部清空）
    pub complete: bool,
    /// 进程最终退出码
    pub exit_code: Option<i32>,
    /// 是否被 Linux OOM Killer 强行终止
    pub oom_killed: bool,
    /// 退出期间发生的异常条件列表
    pub conditions: Vec<ProcessCondition>,
    /// 退出原因代码
    pub reason_code: String,
}

/// 进程停止请求参数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRequest {
    /// 优雅停机宽限期（超时后升级为 SIGKILL 强杀）
    pub grace_period: Duration,
    /// 是否跳过优雅停机直接立即强杀
    pub immediate: bool,
}
