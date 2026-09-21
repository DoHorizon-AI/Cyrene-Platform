//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_observability                                           │
//! │  Role: Thin observability, structured logging, and diagnostics.    │
//! │                                                                     │
//! │  模块职责：Cyrene Platform 最薄可观测性、结构化日志与诊断共享辅助库。       │
//! └─────────────────────────────────────────────────────────────────────┘

pub mod config;
pub mod error_catalog;
pub mod events;
pub mod formatter;
pub mod guard;
pub mod init;
pub mod panic_hook;
pub mod redaction;

#[cfg(test)]
mod tests;

pub use config::{LogFormat, ObservabilityConfig};
pub use error_catalog::PlatformErrorCode;
pub use events::*;
pub use formatter::CyreneLayer;
pub use guard::ObservabilityGuard;
pub use init::{init_observability, ObservabilityError};
pub use redaction::{
    is_sensitive_key, sanitize_field, truncate_bounded, DEFAULT_MAX_CAUSE_DEPTH,
    DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_MAX_RECORD_BYTES, REDACTED_MARKER, TRUNCATED_MARKER,
};

/// ════════════════════════════════════════════════════════════════════════
/// Emits a structured Cyrene Platform event at the specified level.
/// ════════════════════════════════════════════════════════════════════════
#[macro_export]
macro_rules! emit_event {
    ($level:ident, $event_name:expr, $msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        ::tracing::$level!(
            event.name = $event_name,
            message = $msg,
            $($key = $val,)*
        )
    };
}

/// ════════════════════════════════════════════════════════════════════════
/// Emits a structured Cyrene Platform error event with an explicit error code.
/// ════════════════════════════════════════════════════════════════════════
#[macro_export]
macro_rules! emit_error {
    ($event_name:expr, $err_code:expr, $msg:expr $(, $key:ident = $val:expr)* $(,)?) => {
        ::tracing::error!(
            event.name = $event_name,
            error.code = $err_code.as_str(),
            message = $msg,
            $($key = $val,)*
        )
    };
}
