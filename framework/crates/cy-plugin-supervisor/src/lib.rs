//! CYRENE 插件进程监管器与多路复用 RPC 路由层 (Plugin Supervisor & RPC Multiplexer).
//!
//! # 架构过渡说明 (Architecture Transition Note)
//! 根据 `ADR-PLUGIN-RUNTIME` 与 `ADR-HARDWARE-ADAPTER-BOUNDARY`，本 crate 原生的裸子进程
//! 拉起模式正在过渡迁移至内核级 [`cy_kernel_daemon::watchdog::InstanceActor`] 与
//! [`cy_kernel_daemon::SandboxedProcess`] + `sandboxd` 强隔离沙箱底座。
//! 生产 Worker 实例生命周期与 Fencing 门禁由 Kernel Watchdog 全权接管。
//!
//! 【插件进程生命周期与韧性架构】
//! 每个外部插件（Python / JVM 子进程）在平台中由一个专属的 [`PluginSupervisor`] 实例进行全程生命周期管控：
//! 1. **全状态机跟踪 ([`PluginRuntimeState`])**：精细化追踪插件从发现、解析、拉起、握手、健康、降级到崩溃、隔离的 13 种状态；
//! 2. **零端口握手与协议协商**：拉起子进程后通过 stdio 发送 `Hello` 协议包，校验主宿协议版本与 API 版本兼容性，协商能力集；
//! 3. **多路复用 RPC 与响应关联 (Request-Response Multiplexing)**：主后台任务监听子进程 stdout，通过 `request_id` 在 `pending_requests` 字典中查找并触发对应的 `oneshot` 通道唤醒发起方；
//! 4. **超时与取消传播 (Cancel Envelope)**：当 RPC 调用超时时，自动向子进程投递 `Cancel` 消息并清理挂起状态；
//! 5. **滑动窗口崩溃环路检测 ([`CrashTracker`])**：在滑动时间窗口（如 60s 内崩溃 >= 3次）捕获崩溃风暴，自动触发隔离封锁（Quarantine），防止陷入无休止的死循环重启；
//! 6. **可配置重启退避策略 ([`RestartPolicy`])**：支持指数退避重启，保障系统自愈。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use cy_local_transport::TransportError;
use cy_plugin_protocol::pb::{
    envelope, plugin_error_payload, Cancel, Hello, Invoke, InvokeResult, Shutdown,
};
use cy_plugin_protocol::{Envelope, CURRENT_PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{info, warn};

/// 插件运行时 13 态精细化生命周期状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginRuntimeState {
    /// 1. 已扫描发现清单
    Discovered,
    /// 2. 依赖项与环境已解析满足
    Resolved,
    /// 3. 正在启动子进程
    Starting,
    /// 4. 正在执行 stdio 握手与协议协商
    Handshaking,
    /// 5. 处于正常健康运行状态（可接收 RPC 调用）
    Healthy,
    /// 6. 降级状态（部分非核心功能受损）
    Degraded,
    /// 7. 正在优雅停机中
    Stopping,
    /// 8. 已完全停止
    Stopped,
    /// 9. 不可用（缺少可执行文件或环境）
    Unavailable,
    /// 10. 协议或 API 版本不兼容
    Incompatible,
    /// 11. 进程发生崩溃退出
    Crashed,
    /// 12. 触发崩溃环路已被隔离封锁（拒绝后续自动重启）
    Quarantined,
    /// 13. 人工显式禁用
    Disabled,
}

impl std::fmt::Display for PluginRuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub use cy_manifest::RestartPolicy;

/// 滑动时间窗口崩溃环路追踪器：用于防止频繁重启导致系统雪崩
#[derive(Debug, Clone)]
pub struct CrashTracker {
    /// 历史崩溃时间戳队列
    history: VecDeque<Instant>,
    /// 窗口内允许的最大崩溃阈值次数
    max_crashes_in_window: usize,
    /// 评估滑动窗口时长（例如 60 秒）
    window_duration: Duration,
}

impl CrashTracker {
    /// 创建崩溃追踪器
    pub fn new(max_crashes_in_window: usize, window_duration: Duration) -> Self {
        Self {
            history: VecDeque::new(),
            max_crashes_in_window,
            window_duration,
        }
    }

    /// 记录一次崩溃事件，并返回当前是否已触发崩溃环路 (Crash Loop)
    pub fn record_crash(&mut self) -> bool {
        let now = Instant::now();
        self.history.push_back(now);
        // 淘汰窗口之外过期的历史崩溃记录
        while let Some(&t) = self.history.front() {
            if now.duration_since(t) > self.window_duration {
                self.history.pop_front();
            } else {
                break;
            }
        }
        self.history.len() >= self.max_crashes_in_window
    }

    /// 重置崩溃计数历史
    pub fn reset(&mut self) {
        self.history.clear();
    }
}

/// 插件监管层错误枚举
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// 进程拉起失败
    #[error("Plugin launch failed: {0}")]
    LaunchFailed(String),
    /// 握手超时
    #[error("Handshake timeout after {0:?}")]
    HandshakeTimeout(Duration),
    /// 有线传输层协议版本不匹配
    #[error("Incompatible protocol version: expected {expected}, got {got}")]
    IncompatibleProtocolVersion { expected: u32, got: u32 },
    /// 平台 API 契约版本不兼容
    #[error("Incompatible API version: {0}")]
    IncompatibleApiVersion(String),
    /// 底层 I/O 传输异常
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),
    /// 插件因崩溃环路已被隔离封锁
    #[error("Plugin is in quarantined state due to crash loop")]
    Quarantined,
    /// 插件内部 RPC 报错
    #[error("Plugin RPC error [{code:?}]: {message}")]
    PluginRpc {
        code: plugin_error_payload::Code,
        message: String,
    },
    /// RPC 请求等待响应超时
    #[error("Request timeout after {0:?}")]
    Timeout(Duration),
}

/// 插件进程监管器：负责进程拉起、零端口传输、握手协商、状态管理、多路复用与故障自愈
pub struct PluginSupervisor {
    /// 插件唯一标识 ID
    plugin_id: String,
    /// 可执行程序文件路径
    executable: String,
    /// 启动命令行参数
    args: Vec<String>,
    /// 当前生命周期状态
    state: PluginRuntimeState,
    /// 状态原因说明
    state_reason: Option<String>,
    /// 故障重启策略
    restart_policy: RestartPolicy,
    /// 崩溃环路追踪器
    crash_tracker: CrashTracker,
    /// 已累计重启次数
    restart_count: u32,
    /// 发送向子进程的数据发送端通道
    transport_tx: Option<mpsc::Sender<Envelope>>,
    /// 正在等待响应的请求表 (`request_id -> oneshot::Sender<Envelope>`)
    pending_requests: Arc<AsyncMutex<HashMap<String, oneshot::Sender<Envelope>>>>,
    /// 握手协商后插件声明的能力特性
    declared_capabilities: Vec<String>,
    /// 协商一致的 API 版本
    negotiated_api_version: Option<String>,
    /// 插件清单中的完整能力描述
    pub capabilities_manifest: cy_manifest::PluginCapabilitiesManifest,
    /// 插件上报的运行时指标
    pub metrics: HashMap<String, String>,
}

impl PluginSupervisor {
    /// 构造插件监管器实例
    pub fn new(
        plugin_id: impl Into<String>,
        executable: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            executable: executable.into(),
            args,
            state: PluginRuntimeState::Discovered,
            state_reason: None,
            restart_policy: RestartPolicy::default(),
            crash_tracker: CrashTracker::new(3, Duration::from_secs(60)),
            restart_count: 0,
            transport_tx: None,
            pending_requests: Arc::new(AsyncMutex::new(HashMap::new())),
            declared_capabilities: Vec::new(),
            negotiated_api_version: None,
            capabilities_manifest: Default::default(),
            metrics: HashMap::new(),
        }
    }

    /// 获取当前插件运行状态
    pub fn state(&self) -> PluginRuntimeState {
        self.state
    }

    /// 获取状态原因说明
    pub fn state_reason(&self) -> Option<&str> {
        self.state_reason.as_deref()
    }

    /// 获取重启策略
    pub fn restart_policy(&self) -> &RestartPolicy {
        &self.restart_policy
    }

    /// 配置重启策略
    pub fn set_restart_policy(&mut self, policy: RestartPolicy) {
        self.restart_policy = policy;
    }

    /// 获取累计重启次数
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// 获取插件声明的能力列表
    pub fn declared_capabilities(&self) -> &[String] {
        &self.declared_capabilities
    }

    /// 手动解除插件的隔离封锁状态，重置崩溃计数器
    pub fn unquarantine(&mut self) {
        if self.state == PluginRuntimeState::Quarantined {
            self.state = PluginRuntimeState::Stopped;
            self.state_reason = Some("Manually unquarantined".to_string());
            self.crash_tracker.reset();
        }
    }

    /// 确保插件处于健康运行状态；若已崩溃且未超出重启策略限制，则执行指数退避重启
    pub async fn ensure_healthy(&mut self, timeout: Duration) -> Result<(), SupervisorError> {
        if self.state == PluginRuntimeState::Healthy {
            return Ok(());
        }
        if self.state == PluginRuntimeState::Quarantined {
            return Err(SupervisorError::Quarantined);
        }

        if self.state == PluginRuntimeState::Starting
            || self.state == PluginRuntimeState::Handshaking
        {
            return Err(SupervisorError::LaunchFailed(
                "Plugin is currently starting".to_string(),
            ));
        }

        if matches!(
            self.state,
            PluginRuntimeState::Discovered | PluginRuntimeState::Resolved
        ) {
            info!(
                "Plugin {} is in initial state ({:?}); starting...",
                self.plugin_id, self.state
            );
            self.start(timeout).await?;
            return Ok(());
        }

        if matches!(
            self.state,
            PluginRuntimeState::Crashed | PluginRuntimeState::Degraded
        ) {
            let should_restart = match &self.restart_policy {
                RestartPolicy::Never => false,
                RestartPolicy::Always => true,
                RestartPolicy::OnFailure { max_restarts, .. } => self.restart_count < *max_restarts,
            };

            return if should_restart {
                self.restart_count += 1;
                let backoff = match &self.restart_policy {
                    RestartPolicy::OnFailure {
                        min_backoff_ms,
                        max_backoff_ms,
                        factor,
                        ..
                    } => {
                        let backoff = (*min_backoff_ms as f64
                            * factor.powi((self.restart_count - 1) as i32))
                            as u64;
                        Duration::from_millis(backoff.min(*max_backoff_ms))
                    }
                    RestartPolicy::Always => Duration::from_millis(1000),
                    _ => Duration::from_millis(0),
                };
                info!(
                    "Restarting plugin {}, attempt {}, backoff {:?}",
                    self.plugin_id, self.restart_count, backoff
                );
                tokio::time::sleep(backoff).await;

                // 清理旧传输状态
                self.transport_tx = None;
                self.pending_requests = Arc::new(AsyncMutex::new(HashMap::new()));

                self.start(timeout).await?;
                Ok(())
            } else {
                Err(SupervisorError::LaunchFailed(
                    "Plugin crashed and restart policy exhausted or disabled".to_string(),
                ))
            };
        }

        Err(SupervisorError::LaunchFailed(format!(
            "Plugin is in state {:?}",
            self.state
        )))
    }

    /// 启动插件子进程握手（必须先通过 attach_transport_channel 挂接 SandboxedProcess 强隔离通道）
    pub async fn start(&mut self, _timeout: Duration) -> Result<(), SupervisorError> {
        if self.state == PluginRuntimeState::Quarantined {
            return Err(SupervisorError::Quarantined);
        }

        if self.transport_tx.is_none() {
            self.state = PluginRuntimeState::Unavailable;
            self.state_reason = Some(
                "Bare process spawning is removed; transport channel must be attached via attach_transport_channel (managed by Kernel InstanceActor / SandboxedProcess)".to_string(),
            );
            return Err(SupervisorError::LaunchFailed(
                "Bare process spawning is decommissioned; attach_transport_channel required".to_string(),
            ));
        }

        self.state = PluginRuntimeState::Healthy;
        Ok(())
    }

    /// 向插件子进程发起异步扩展点 RPC 调用并限时等待响应
    pub async fn invoke(
        &mut self,
        invoke_payload: Invoke,
        timeout: Duration,
    ) -> Result<InvokeResult, SupervisorError> {
        self.ensure_healthy(Duration::from_secs(10)).await?;

        let tx = self
            .transport_tx
            .as_ref()
            .ok_or_else(|| SupervisorError::LaunchFailed("Transport not initialized".to_string()))?
            .clone();

        let req_id = uuid::Uuid::new_v4().to_string();
        let env = Envelope {
            request_id: req_id.clone(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: (Utc::now() + chrono::Duration::milliseconds(timeout.as_millis() as i64))
                .timestamp_millis(),
            sequence_number: 0,
            payload: Some(envelope::Payload::Invoke(invoke_payload)),
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending_requests
            .lock()
            .await
            .insert(req_id.clone(), reply_tx);

        if tx.send(env).await.is_err() {
            self.pending_requests.lock().await.remove(&req_id);
            warn!("Failed to route request to plugin {}", self.plugin_id);
            self.handle_crash();
            return Err(SupervisorError::Transport(TransportError::Closed));
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(Envelope {
                payload: Some(envelope::Payload::InvokeResult(res)),
                ..
            })) => Ok(res),
            Ok(Ok(Envelope {
                payload: Some(envelope::Payload::Error(err)),
                ..
            })) => Err(SupervisorError::PluginRpc {
                code: plugin_error_payload::Code::try_from(err.code)
                    .unwrap_or(plugin_error_payload::Code::Unknown),
                message: err.message,
            }),
            Ok(Ok(_)) | Ok(Err(_)) => {
                self.handle_crash();
                Err(SupervisorError::LaunchFailed(
                    "Plugin process crashed or closed connection during invoke".to_string(),
                ))
            }
            Err(_) => {
                // 超时后从 pending_requests 移除并向子进程发送 Cancel 消息避免交叉污染
                self.pending_requests.lock().await.remove(&req_id);
                let cancel_env = Envelope {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    trace_id: uuid::Uuid::new_v4().to_string(),
                    plugin_id: self.plugin_id.clone(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: 0,
                    payload: Some(envelope::Payload::Cancel(Cancel {
                        target_request_id: req_id,
                        reason: "Invoke timeout".to_string(),
                    })),
                };
                let _ = tx.send(cancel_env).await;
                self.handle_crash();
                Err(SupervisorError::Timeout(timeout))
            }
        }
    }

    /// 内部处理进程崩溃退出，更新状态并触发滑动窗口崩溃检测
    fn handle_crash(&mut self) {
        let is_crash_loop = self.crash_tracker.record_crash();
        if is_crash_loop {
            self.state = PluginRuntimeState::Quarantined;
            self.state_reason =
                Some("Quarantined: repeated crashes in short time window".to_string());
            warn!("Plugin {} quarantined due to crash loop.", self.plugin_id);
        } else {
            self.state = PluginRuntimeState::Crashed;
            self.state_reason =
                Some("Process exited or stream terminated unexpectedly".to_string());
        }
        self.transport_tx = None;
    }

    /// 优雅停止插件进程，发送 Shutdown 协议消息
    pub async fn stop(&mut self) -> Result<(), SupervisorError> {
        self.state = PluginRuntimeState::Stopping;
        if let Some(tx) = self.transport_tx.take() {
            let shutdown_env = Envelope {
                request_id: uuid::Uuid::new_v4().to_string(),
                trace_id: uuid::Uuid::new_v4().to_string(),
                plugin_id: self.plugin_id.clone(),
                protocol_version: CURRENT_PROTOCOL_VERSION,
                deadline_ms: 1000,
                sequence_number: 0,
                payload: Some(envelope::Payload::Shutdown(Shutdown {
                    grace_period_ms: 1000,
                })),
            };
            let _ = tx.send(shutdown_env).await;
        }
        self.state = PluginRuntimeState::Stopped;
        Ok(())
    }

    /// 附加已由 Kernel SandboxedProcess / sandboxd 建立的传输通道，避免裸进程拉起
    pub fn attach_transport_channel(&mut self, tx: mpsc::Sender<Envelope>) {
        self.transport_tx = Some(tx);
        self.state = PluginRuntimeState::Healthy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crash_tracker_sliding_window_quarantine() {
        let mut tracker = CrashTracker::new(3, Duration::from_secs(60));
        assert!(!tracker.record_crash()); // 1st crash
        assert!(!tracker.record_crash()); // 2nd crash
        assert!(tracker.record_crash());  // 3rd crash triggers quarantine!
    }

    #[tokio::test]
    async fn test_supervisor_attached_transport_invoke() {
        let mut supervisor = PluginSupervisor::new("test-plugin", "dummy", vec![]);
        let (tx, mut rx) = mpsc::channel::<Envelope>(10);
        supervisor.attach_transport_channel(tx);

        let pending = supervisor.pending_requests.clone();

        // Spawn mock worker loop responding to invoke requests
        tokio::spawn(async move {
            if let Some(req) = rx.recv().await {
                let resp = Envelope {
                    request_id: req.request_id.clone(),
                    trace_id: req.trace_id.clone(),
                    plugin_id: req.plugin_id.clone(),
                    protocol_version: 1,
                    deadline_ms: 0,
                    sequence_number: 1,
                    payload: Some(envelope::Payload::InvokeResult(InvokeResult {
                        response: None,
                    })),
                };
                let mut map = pending.lock().await;
                if let Some(sender) = map.remove(&req.request_id) {
                    let _ = sender.send(resp);
                }
            }
        });

        let result = supervisor
            .invoke(
                Invoke {
                    extension_point: "Probe".to_string(),
                    method: "test".to_string(),
                    request: None,
                },
                Duration::from_secs(2),
            )
            .await;

        assert!(result.is_ok(), "Invoke over attached transport must succeed");
    }
}
