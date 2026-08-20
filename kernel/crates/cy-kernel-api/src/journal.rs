//! 运行时持久化日志记录与端口.

use crate::error::ProviderError;

/// A compact lifecycle record that the pure Kernel can emit to an outer
/// durable journal. The record deliberately carries only node identity,
/// fencing and terminal lifecycle facts; it never serializes launch commands,
/// environment variables, driver data, or Worker payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJournalRecord {
    pub event: RuntimeJournalEvent,
    pub node_id: String,
    pub node_epoch: u64,
    pub instance_name: Option<String>,
    pub lease_name: Option<String>,
    pub fence_token: Option<u64>,
    pub reason_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeJournalEvent {
    KernelStarted,
    LeaseReserved,
    LeaseReleaseStarted,
    LeaseReleased,
    InstanceLaunched,
    InstanceTerminated,
    InstanceCleanupFailed,
    WatchdogReaped,
}

/// Outer runtime persistence port. Implementations belong in `runtime/` or an
/// external service; Kernel decision code never opens journal files itself.
pub trait RuntimeJournalSink: Send + Sync {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError>;
}

#[derive(Debug, Default)]
pub struct NoopRuntimeJournal;

impl RuntimeJournalSink for NoopRuntimeJournal {
    fn append(&self, _record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// Test double whose `append` always fails. Used to exercise the durable-journal
/// failure paths: a lease (or release) must never become externally visible when
/// the fence record cannot be persisted, so the failure must surface and the
/// in-memory state must be rolled back instead of being silently ignored.
#[derive(Debug, Default)]
pub struct FailingRuntimeJournal;

impl RuntimeJournalSink for FailingRuntimeJournal {
    fn append(&self, _record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        Err(ProviderError::new(
            "failing-journal",
            "JOURNAL_WRITE_FAILED",
            "injected durable runtime journal write failure",
        ))
    }
}
