//! 运行时持久化日志记录与端口.

use crate::{error::ProviderError, runtime::RuntimeProcessEvidence, semantic};

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
    /// Local sandbox identity used only by durable restart recovery. It is not
    /// projected through the semantic contract or Core v1/v2 APIs.
    pub runtime_evidence: Option<RuntimeProcessEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeJournalEvent {
    KernelStarted,
    LeaseReserved,
    LeaseReleaseStarted,
    LeaseReleased,
    LeaseRevoked,
    FenceAdvanced,
    /// Durable pre-launch intent, persisted BEFORE the physical spawn. Recovery
    /// classifies a lease that has this record but no `InstanceLaunched` as
    /// "launch intended, outcome unknown" and must not treat the resource as
    /// reusable without discovering/reaping the domain.
    InstanceLaunching,
    InstanceLaunched,
    InstanceTerminated,
    InstanceCleanupFailed,
    WatchdogReaped,
    WorkerLost,
    OperationLost,
    EndpointRevoked,
}

/// Outer runtime persistence port. Implementations belong in `runtime/` or an
/// external service; Kernel decision code never opens journal files itself.
pub trait RuntimeJournalSink: Send + Sync {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError>;
}

/// Append-only event persistence port. Semantic authority never depends on a
/// file, database, or message broker implementation; the composition root
/// selects the durable store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableEventRecord {
    pub namespace: String,
    pub event: semantic::Event,
}

pub trait DurableEventStore: Send + Sync {
    fn append_event(&self, record: DurableEventRecord) -> Result<(), ProviderError>;
    /// `Some`, including an empty history, makes this store the canonical
    /// source for the requested replay. `None` means durable replay is not
    /// supported and callers may use their bounded local history instead.
    fn events_for_source(
        &self,
        _source: &semantic::Identity,
        _namespace: &str,
    ) -> Result<Option<Vec<DurableEventRecord>>, ProviderError> {
        Ok(None)
    }
}

#[derive(Debug, Default)]
pub struct NoopRuntimeJournal;

impl RuntimeJournalSink for NoopRuntimeJournal {
    fn append(&self, _record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        Ok(())
    }
}

impl DurableEventStore for NoopRuntimeJournal {
    fn append_event(&self, _record: DurableEventRecord) -> Result<(), ProviderError> {
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
