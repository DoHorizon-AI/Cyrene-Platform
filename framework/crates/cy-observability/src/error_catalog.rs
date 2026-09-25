//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 error_catalog.rs                                                │
//! │  Module: cy_observability::error_catalog                            │
//! │  Role: Canonical platform error code namespace and definitions.     │
//! │                                                                     │
//! │  模块职责：Platform 第一批稳定错误码命名空间与权威定义。                  │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fmt;

/// ════════════════════════════════════════════════════════════════════════
/// Stable Platform Error Code representation (<OWNER>.<DOMAIN>.<REASON>).
///
/// 平台稳定机器错误码。格式严格为 PLATFORM.<DOMAIN>.<REASON>。
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformErrorCode {
    // Kernel Lifecycle & Recovery
    // 中文：Kernel 生命周期与恢复错误。
    KernelStartupFailed,
    KernelRecoveryFailed,
    KernelJournalWriteFailed,
    KernelUnknownError,

    // Lease & Fencing
    // 中文：Lease 与 fencing 错误。
    LeaseAcquireFailed,
    LeaseReleaseRollbackFailed,
    LeaseReleaseIntentPersistFailed,
    LeaseExpired,
    LeaseStaleGeneration,

    // Worker Lifecycle
    // 中文：Worker 生命周期错误。
    WorkerLaunchFailed,
    WorkerCleanupFailed,
    WorkerCleanupUnconfirmed,
    WorkerHeartbeatTimeout,
    WorkerLostAuthority,

    // Node & Communication
    // 中文：Node 与通信错误。
    NodeConnectFailed,
    NodeDisconnected,
    NodePeerRejected,
    NodeReconnectExhausted,

    // Sandbox & Isolation
    // 中文：Sandbox 与隔离错误。
    SandboxCgroupInitFailed,
    SandboxKillFailed,
    SandboxProcessSpawnFailed,
    SandboxOversizedFrame,

    // Package Runtime & Plugins
    // 中文：Package runtime 与 Plugin 错误。
    PackageActivationFailed,
    PackageDeactivationFailed,
    PackageRollbackFailed,
    PackageStageFailed,

    // Relay & Transport
    // 中文：Relay 与传输错误。
    RelayFrameError,
    RelayStreamDisconnected,
    RelayConnectionRejected,

    // Panic
    // 中文：Panic 错误。
    PanicUnhandled,
}

impl PlatformErrorCode {
    /// Returns the canonical machine string (e.g. "PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED").
    /// 中文：返回规范机器字符串，例如 PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::KernelStartupFailed => "PLATFORM.KERNEL.STARTUP_FAILED",
            Self::KernelRecoveryFailed => "PLATFORM.KERNEL.RECOVERY_FAILED",
            Self::KernelJournalWriteFailed => "PLATFORM.KERNEL.JOURNAL_WRITE_FAILED",
            Self::KernelUnknownError => "PLATFORM.KERNEL.UNKNOWN_ERROR",

            Self::LeaseAcquireFailed => "PLATFORM.LEASE.ACQUIRE_FAILED",
            Self::LeaseReleaseRollbackFailed => "PLATFORM.LEASE.RELEASE_ROLLBACK_FAILED",
            Self::LeaseReleaseIntentPersistFailed => "PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED",
            Self::LeaseExpired => "PLATFORM.LEASE.EXPIRED",
            Self::LeaseStaleGeneration => "PLATFORM.LEASE.STALE_GENERATION",

            Self::WorkerLaunchFailed => "PLATFORM.WORKER.LAUNCH_FAILED",
            Self::WorkerCleanupFailed => "PLATFORM.WORKER.CLEANUP_FAILED",
            Self::WorkerCleanupUnconfirmed => "PLATFORM.WORKER.CLEANUP_UNCONFIRMED",
            Self::WorkerHeartbeatTimeout => "PLATFORM.WORKER.HEARTBEAT_TIMEOUT",
            Self::WorkerLostAuthority => "PLATFORM.WORKER.LOST_AUTHORITY",

            Self::NodeConnectFailed => "PLATFORM.NODE.CONNECT_FAILED",
            Self::NodeDisconnected => "PLATFORM.NODE.DISCONNECTED",
            Self::NodePeerRejected => "PLATFORM.NODE.PEER_REJECTED",
            Self::NodeReconnectExhausted => "PLATFORM.NODE.RECONNECT_EXHAUSTED",

            Self::SandboxCgroupInitFailed => "PLATFORM.SANDBOX.CGROUP_INIT_FAILED",
            Self::SandboxKillFailed => "PLATFORM.SANDBOX.KILL_FAILED",
            Self::SandboxProcessSpawnFailed => "PLATFORM.SANDBOX.PROCESS_SPAWN_FAILED",
            Self::SandboxOversizedFrame => "PLATFORM.SANDBOX.OVERSIZED_FRAME",

            Self::PackageActivationFailed => "PLATFORM.PACKAGE.ACTIVATION_FAILED",
            Self::PackageDeactivationFailed => "PLATFORM.PACKAGE.DEACTIVATION_FAILED",
            Self::PackageRollbackFailed => "PLATFORM.PACKAGE.ROLLBACK_FAILED",
            Self::PackageStageFailed => "PLATFORM.PACKAGE.STAGE_FAILED",

            Self::RelayFrameError => "PLATFORM.RELAY.FRAME_ERROR",
            Self::RelayStreamDisconnected => "PLATFORM.RELAY.STREAM_DISCONNECTED",
            Self::RelayConnectionRejected => "PLATFORM.RELAY.CONNECTION_REJECTED",

            Self::PanicUnhandled => "PLATFORM.PANIC.UNHANDLED",
        }
    }

    /// Domain category for this error.
    /// 中文：此错误所属的 domain 类别。
    pub fn domain(&self) -> &'static str {
        match self {
            Self::KernelStartupFailed
            | Self::KernelRecoveryFailed
            | Self::KernelJournalWriteFailed
            | Self::KernelUnknownError => "KERNEL",

            Self::LeaseAcquireFailed
            | Self::LeaseReleaseRollbackFailed
            | Self::LeaseReleaseIntentPersistFailed
            | Self::LeaseExpired
            | Self::LeaseStaleGeneration => "LEASE",

            Self::WorkerLaunchFailed
            | Self::WorkerCleanupFailed
            | Self::WorkerCleanupUnconfirmed
            | Self::WorkerHeartbeatTimeout
            | Self::WorkerLostAuthority => "WORKER",

            Self::NodeConnectFailed
            | Self::NodeDisconnected
            | Self::NodePeerRejected
            | Self::NodeReconnectExhausted => "NODE",

            Self::SandboxCgroupInitFailed
            | Self::SandboxKillFailed
            | Self::SandboxProcessSpawnFailed
            | Self::SandboxOversizedFrame => "SANDBOX",

            Self::PackageActivationFailed
            | Self::PackageDeactivationFailed
            | Self::PackageRollbackFailed
            | Self::PackageStageFailed => "PACKAGE",

            Self::RelayFrameError
            | Self::RelayStreamDisconnected
            | Self::RelayConnectionRejected => "RELAY",

            Self::PanicUnhandled => "PANIC",
        }
    }
}

impl fmt::Display for PlatformErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
