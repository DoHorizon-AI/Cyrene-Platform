// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/service.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Service and Workload Supervision Specification.
//! 通用 Service 和 Workload 监督规范。
//!
//! Provides Product-neutral definitions for hosting long-running services,
//! including lifecycle states, readiness probes, deterministic restart
//! backoff policies, endpoint bindings, and structured lifecycle events.
//! 为长期运行的服务提供与 Product 无关的定义，包括生命周期状态、就绪探测、确定性重启退避策略、端点绑定和结构化生命周期事件。
//!
//! # Architectural Invariant (ADR-010):
//! # 架构不变量（ADR-010）：
//!
//! `ServiceSpec`, `ServiceState`, `ServiceStatus`, and `ServiceEvent` are
//! **daemon-level process supervision & orchestration abstractions**.
//! They are **NOT** authoritative Kernel semantic entities.
//! 这些类型是 **daemon 层的进程监督与编排抽象**，**不是** Kernel 语义权威实体。
//!
//! - **NO** `ServiceId` resource in `cy-kernel-contract`.
//! - **不得**在 `cy-kernel-contract` 中添加 `ServiceId` resource。
//! - **NO** `ServiceRepository`, `ServiceLedger`, or `Service` persistence in Kernel authority.
//! - **不得**在 Kernel authority 中添加 `ServiceRepository`、`ServiceLedger` 或 `Service` 持久化。
//! - **NO** independent authoritative `Service` event stream in the authority ledger.
//! - **不得**在 authority ledger 中另设权威 `Service` 事件流。
//!
//! Service workloads compose existing Kernel primitives: [`LaunchPlan`],
//! [`crate::ProcessRuntime`], [`crate::SandboxBackend`], [`CleanupReport`], and [`semantic::Endpoint`].
//! Service workload 组合使用现有 Kernel 原语：[`LaunchPlan`]、[`crate::ProcessRuntime`]、[`crate::SandboxBackend`]、[`CleanupReport`] 和 [`semantic::Endpoint`]。

use std::{collections::BTreeMap, time::Duration};

use cy_kernel_contract as semantic;

use crate::{launch::LaunchPlan, runtime::CleanupReport};

/// Bounded exponential backoff configuration for process restarts.
/// 用于进程重启的有界指数退避配置。
#[derive(Debug, Clone, PartialEq)]
pub struct BackoffConfig {
    /// Initial backoff delay before the first restart attempt.
    /// 首次重启尝试前的初始退避时长。
    pub initial_delay: Duration,
    /// Maximum backoff ceiling.
    /// 退避时长上限。
    pub max_delay: Duration,
    /// Multiplier factor applied on successive restart attempts.
    /// 后续重启尝试使用的倍率。
    pub multiplier: f64,
    /// Duration of continuous healthy execution required to reset the restart counter.
    /// 重置重启计数器所需的连续健康运行时长。
    pub reset_after: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }
}

impl BackoffConfig {
    /// Create an exponential backoff configuration with explicit parameters.
    /// 使用显式参数创建指数退避配置。
    pub fn exponential(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            max_delay,
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }

    /// Compute the deterministic backoff delay for a given 1-based attempt index.
    /// 根据从 1 开始的尝试序号计算确定性的退避时长。
    pub fn compute_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exp = attempt.saturating_sub(1);
        let factor = self.multiplier.powi(exp as i32);
        let delay_nanos = (self.initial_delay.as_nanos() as f64 * factor) as u128;
        let delay = Duration::from_nanos(delay_nanos.min(u64::MAX as u128) as u64);
        delay.min(self.max_delay)
    }
}

/// Service restart policy controlling supervisor behavior on process exit.
/// 控制进程退出后 supervisor 行为的 Service 重启策略。
#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    /// Never restart the service process once it exits or fails.
    /// Service 进程退出或失败后不再重启。
    Never,
    /// Always restart the service regardless of exit code (clean exit or crash).
    /// 无论退出码如何（正常退出或崩溃），始终重启 Service。
    Always {
        /// Optional ceiling on consecutive restart attempts.
        /// 连续重启尝试次数的可选上限。
        max_retries: Option<u32>,
        /// Exponential backoff policy.
        /// 指数退避策略。
        backoff: BackoffConfig,
    },
    /// Restart only when the process exits unexpectedly or with a non-zero exit code.
    /// 仅当进程意外退出或以非零退出码结束时重启。
    OnFailure {
        /// Optional ceiling on consecutive restart attempts.
        /// 连续重启尝试次数的可选上限。
        max_retries: Option<u32>,
        /// Exponential backoff policy.
        /// 指数退避策略。
        backoff: BackoffConfig,
    },
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::OnFailure {
            max_retries: Some(5),
            backoff: BackoffConfig::default(),
        }
    }
}

/// Generic readiness probe mechanism to determine when a service is ready to accept traffic.
/// 用于判断 Service 是否已就绪并可接收流量的通用探测机制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessProbe {
    /// Process is alive and running in the OS / sandbox.
    /// 进程在操作系统或 sandbox 中存活且正在运行。
    ProcessAlive,
    /// TCP connection can be established to the specified host and port.
    /// 可以连接到指定 host 和 port 的 TCP 端点。
    TcpSocket { host: String, port: u16 },
    /// HTTP GET request returns an acceptable status code (default 200..=299).
    /// HTTP GET 请求返回可接受的状态码（默认范围为 200..=299）。
    HttpGet {
        host: String,
        port: u16,
        path: String,
        expected_status: Option<u16>,
    },
    /// WorkerControl bidirectional handshake (Hello/HelloAck) completed.
    /// WorkerControl 双向握手（Hello/HelloAck）已完成。
    WorkerControl,
}

/// Timing and retry parameters for readiness probe execution.
/// 就绪探测的时序与重试参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeConfig {
    /// Delay before the first probe attempt after process launch.
    /// 进程启动后首次探测前的延迟。
    pub initial_delay: Duration,
    /// Period between subsequent probe checks.
    /// 后续探测检查之间的间隔。
    pub period: Duration,
    /// Timeout for each individual probe check.
    /// 每次探测检查的超时时间。
    pub timeout: Duration,
    /// Number of consecutive successes required to declare readiness.
    /// 判定就绪所需的连续成功次数。
    pub success_threshold: u32,
    /// Number of consecutive failures before declaring readiness probe failure.
    /// 判定就绪探测失败前允许的连续失败次数。
    pub failure_threshold: u32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(10),
            period: Duration::from_millis(50),
            timeout: Duration::from_millis(500),
            success_threshold: 1,
            failure_threshold: 60, // allows up to 3.0s for slow runtimes | 为运行较慢的 runtime 留出最多 3.0 秒。
        }
    }
}

/// Specification for an endpoint exposed by the supervised service.
/// 受监督 Service 对外提供的端点规范。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpointSpec {
    /// Endpoint transport mechanism (e.g. "http", "grpc", "tcp", "uds").
    /// 端点传输机制，例如 "http"、"grpc"、"tcp" 或 "uds"。
    pub transport: String,
    /// Schema ID describing the API contract.
    /// 描述 API 契约的 Schema ID。
    pub schema_id: String,
    /// Optional listening port.
    /// 可选的监听端口。
    pub port: Option<u16>,
    /// Optional base path or socket path.
    /// 可选的基础路径或 socket 路径。
    pub path: Option<String>,
    /// Generic public metadata attributes.
    /// 通用公开元数据属性。
    pub attributes: BTreeMap<String, String>,
    /// Opaque location consumed by the direct Product-side transport client.
    /// 供 Product 侧直连传输客户端使用的不透明位置。
    pub connection_ref: String,
    /// Optional secret-provider reference; never a credential value.
    /// 可选的 secret-provider 引用；不得存放凭据值。
    pub credential_ref: Option<String>,
}

/// Complete declarative specification for a generic long-running service workload.
/// 通用长期运行 Service workload 的完整声明式规范。
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSpec {
    /// Unique service instance name.
    /// 唯一的 Service 实例名称。
    pub name: String,
    /// Concrete process launch plan.
    /// 具体的进程启动计划。
    pub plan: LaunchPlan,
    /// Optional readiness probe configuration.
    /// 可选的就绪探测配置。
    pub readiness_probe: Option<(ReadinessProbe, ProbeConfig)>,
    /// Restart and failure policy.
    /// 重启与失败策略。
    pub restart_policy: RestartPolicy,
    /// Maximum grace period to wait for graceful stop before SIGKILL escalation.
    /// 优雅停止后升级发送 SIGKILL 前的最长等待时长。
    pub graceful_stop_timeout: Duration,
    /// Optional service endpoint specification.
    /// 可选的 Service 端点规范。
    pub endpoint: Option<ServiceEndpointSpec>,
}

impl ServiceSpec {
    pub fn new(name: impl Into<String>, plan: LaunchPlan) -> Self {
        Self {
            name: name.into(),
            plan,
            readiness_probe: Some((ReadinessProbe::ProcessAlive, ProbeConfig::default())),
            restart_policy: RestartPolicy::default(),
            graceful_stop_timeout: Duration::from_secs(5),
            endpoint: None,
        }
    }

    pub fn with_readiness_probe(mut self, probe: ReadinessProbe, config: ProbeConfig) -> Self {
        self.readiness_probe = Some((probe, config));
        self
    }

    pub fn with_restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }

    pub fn with_graceful_stop_timeout(mut self, timeout: Duration) -> Self {
        self.graceful_stop_timeout = timeout;
        self
    }

    pub fn with_endpoint(mut self, endpoint: ServiceEndpointSpec) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
}

/// Observable lifecycle state of a supervised service.
/// 可观测的受监督 Service 生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// Process has been launched and is waiting to satisfy readiness probes.
    /// 进程已启动，正在等待通过就绪探测。
    Starting,
    /// Readiness probe succeeded; endpoints published.
    /// 就绪探测成功，端点已发布。
    Ready,
    /// Service is actively executing workload.
    /// Service 正在执行 workload。
    Running,
    /// Graceful stop requested; waiting for process exit within timeout.
    /// 已请求优雅停止，正在等待进程于超时前退出。
    Stopping,
    /// Process exited cleanly and stopped.
    /// 进程正常退出并已停止。
    Stopped,
    /// Process terminated unexpectedly or failed readiness checks.
    /// 进程意外终止或未通过就绪检查。
    Failed,
    /// Process is in backoff delay before being restarted.
    /// 进程正在退避等待，之后将尝试重启。
    Restarting,
    /// Restart budget exhausted or consecutive crash limit reached.
    /// 重启预算已耗尽或连续崩溃次数达到上限。
    Quarantined,
}

impl ServiceState {
    /// Returns true if the service is in a terminal non-running state.
    /// 如果 Service 处于终止且未运行的状态，则返回 true。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed | Self::Quarantined)
    }

    /// Returns true if the service is actively running or making progress.
    /// 如果 Service 正在运行或持续推进，则返回 true。
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Ready | Self::Running | Self::Restarting
        )
    }

    /// Convert state to stable canonical reason code.
    /// 将状态转换为稳定的规范 reason code。
    pub fn as_reason_code(&self) -> &'static str {
        match self {
            Self::Starting => "SERVICE_STARTING",
            Self::Ready => "SERVICE_READY",
            Self::Running => "SERVICE_RUNNING",
            Self::Stopping => "SERVICE_STOPPING",
            Self::Stopped => "SERVICE_STOPPED",
            Self::Failed => "SERVICE_FAILED",
            Self::Restarting => "SERVICE_RESTARTING",
            Self::Quarantined => "SERVICE_QUARANTINED",
        }
    }
}

/// Instantaneous status snapshot of a supervised service.
/// 受监督 Service 的瞬时状态快照。
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    pub restart_count: u32,
    pub last_exit_report: Option<CleanupReport>,
    pub published_endpoint: Option<semantic::Endpoint>,
    pub last_error: Option<String>,
}

/// Immutable structured lifecycle event published by the service supervisor.
/// 由 Service supervisor 发布的不可变结构化生命周期事件。
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceEvent {
    pub service_name: String,
    pub state: ServiceState,
    pub timestamp_unix_ms: u64,
    pub reason_code: String,
    pub message: String,
}
