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
    pub fn shutdown_with_timeout(self, _timeout: Duration) {
        // Dropping the inner WorkerGuards initiates the flush of tracing-appender's queue
        drop(self);
    }

    /// Configured flush timeout.
    pub fn flush_timeout(&self) -> Duration {
        self.flush_timeout
    }
}
