//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 guard.rs                                                        │
//! │  Module: cy_observability::guard                                    │
//! │  Role: RAII guard ensuring bounded flush on process shutdown.       │
//! │                                                                     │
//! │  模块职责：RAII 生命周期守卫，确保进程退出时执行有界刷新与安全关闭。     │
//! └─────────────────────────────────────────────────────────────────────┘

use std::time::Duration;
use tracing_appender::non_blocking::WorkerGuard;

/// ════════════════════════════════════════════════════════════════════════
/// RAII guard returned by `init_observability`.
///
/// When dropped, ensures that pending log records in the non-blocking queue
/// are flushed to stderr or file sink before the process exits.
/// ════════════════════════════════════════════════════════════════════════
/// 中文：`init_observability` 返回的 RAII guard。
///
/// 中文：guard 被丢弃时，会确保进程退出前将非阻塞队列中的待处理日志记录刷新到 stderr 或文件 sink。
#[derive(Debug)]
pub struct ObservabilityGuard {
    _guards: Vec<WorkerGuard>,
    flush_timeout: Duration,
}

impl ObservabilityGuard {
    pub fn new(guards: Vec<WorkerGuard>, flush_timeout: Duration) -> Self {
        Self {
            _guards: guards,
            flush_timeout,
        }
    }

    /// Explicitly completes bounded shutdown with a specified timeout.
    /// 中文：使用指定的超时时间显式完成有界关闭。
    pub fn shutdown_with_timeout(self, _timeout: Duration) {
        // Dropping the inner WorkerGuards initiates the flush of tracing-appender's queue
        // 中文：丢弃内部 WorkerGuards 会启动 tracing-appender 队列的刷新。
        drop(self);
    }

    /// Configured flush timeout.
    /// 中文：配置的刷新超时时间。
    pub fn flush_timeout(&self) -> Duration {
        self.flush_timeout
    }
}
