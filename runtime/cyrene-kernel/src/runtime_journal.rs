// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: runtime/cyrene-kernel/src/runtime_journal.rs
// ║ Module: CYRENE Platform
// ║ Role: Durable Kernel epoch/fence journal and fail-closed restart evidence.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：持久化 Kernel epoch/fence 日志与 fail-closed 重启证据。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Durable restart evidence for the outer Kernel composition root.
//!
//! It records only node epochs, fence tokens, instance names and terminal
//! reasons. It is intentionally not a Worker recovery database: a fresh
//! Kernel process never rehydrates or adopts an old Worker from this file.

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic, DurableEventRecord, DurableEventStore, NamespaceId, ProviderError,
    RuntimeJournalEvent, RuntimeJournalRecord, RuntimeJournalSink, RuntimeProcessEvidence,
    SandboxBackend,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryState {
    /// The newly persisted epoch for this Kernel process.
    pub node_epoch: u64,
    pub next_fence_token: u64,
    /// Evidence for a process that was launched but never durably recorded as
    /// terminated. A restart must classify it as stale and reconcile actual
    /// Provider/runtime reality; it must never silently adopt it.
    pub runtime_processes: Vec<RuntimeProcessRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProcessRecord {
    pub instance_name: String,
    pub lease_name: Option<String>,
    pub fence_token: Option<u64>,
    pub node_epoch: u64,
    pub runtime_evidence: Option<RuntimeProcessEvidence>,
}

/// A local-only recovery verdict. No verdict rehydrates a Worker into the new
/// Kernel process; only stale, exactly verified evidence is eligible for
/// sandbox cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryClassification {
    Valid,
    Stale,
    Unknown,
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub classification: RecoveryClassification,
    pub record: Option<RuntimeProcessRecord>,
    pub observed: Option<RuntimeProcessEvidence>,
}

#[derive(Debug)]
pub struct FileRuntimeJournal {
    path: PathBuf,
    write_lock: Mutex<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRuntimeRecord {
    observed_at_millis: u64,
    event: String,
    node_id: String,
    node_epoch: u64,
    instance_name: Option<String>,
    lease_name: Option<String>,
    fence_token: Option<u64>,
    reason_code: String,
    #[serde(default)]
    runtime_evidence: Option<PersistedRuntimeProcessEvidence>,
    #[serde(default)]
    semantic_event: Option<PersistedSemanticEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRuntimeProcessEvidence {
    cgroup_name: String,
    pid: u32,
    start_time_ticks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSemanticEvent {
    namespace: String,
    sequence: u64,
    source_id: String,
    source_generation: u64,
    subject_id: String,
    subject_generation: u64,
    kind: String,
    observed_at_unix_ms: u64,
    schema_id: String,
    body: Vec<u8>,
}

impl FileRuntimeJournal {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            write_lock: Mutex::new(()),
        })
    }

    pub fn begin_epoch(&self, node_id: &str) -> std::io::Result<RecoveryState> {
        let recovery = self.recover(node_id)?;
        let node_epoch = now_millis().max(recovery.node_epoch.saturating_add(1));
        self.append_persisted(PersistedRuntimeRecord {
            observed_at_millis: now_millis(),
            event: event_name(RuntimeJournalEvent::KernelStarted).to_string(),
            node_id: node_id.to_string(),
            node_epoch,
            instance_name: None,
            lease_name: None,
            fence_token: None,
            reason_code: "KERNEL_EPOCH_STARTED".to_string(),
            runtime_evidence: None,
            semantic_event: None,
        })?;
        Ok(RecoveryState {
            node_epoch,
            next_fence_token: recovery.next_fence_token,
            runtime_processes: recovery.runtime_processes,
        })
    }

    pub fn recover(&self, node_id: &str) -> std::io::Result<RecoveryState> {
        if !self.path.exists() {
            return Ok(RecoveryState {
                node_epoch: 0,
                next_fence_token: 1,
                runtime_processes: Vec::new(),
            });
        }
        let mut node_epoch = 0_u64;
        let mut max_fence_token = 0_u64;
        let mut runtime_processes = std::collections::BTreeMap::new();
        let contents = std::fs::read_to_string(&self.path)?;
        let has_terminal_newline = contents.ends_with('\n');
        let lines = contents.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            // A power loss can leave an incomplete final JSON line. It never
            // represents a completed lifecycle transition, so ignore it; a
            // malformed earlier line is durable evidence corruption and must
            // stop startup rather than silently weakening fencing.
            let record = match serde_json::from_str::<PersistedRuntimeRecord>(line) {
                Ok(record) => record,
                Err(_) if index + 1 == lines.len() && !has_terminal_newline => continue,
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid runtime journal record: {error}"),
                    ));
                }
            };
            if record.node_id == node_id {
                node_epoch = node_epoch.max(record.node_epoch);
                max_fence_token = max_fence_token.max(record.fence_token.unwrap_or(0));
                if let Some(instance_name) = record.instance_name {
                    match record.event.as_str() {
                        "INSTANCE_LAUNCHED" => {
                            runtime_processes.insert(
                                instance_name.clone(),
                                RuntimeProcessRecord {
                                    instance_name,
                                    lease_name: record.lease_name,
                                    fence_token: record.fence_token,
                                    node_epoch: record.node_epoch,
                                    runtime_evidence: record.runtime_evidence.map(|evidence| {
                                        RuntimeProcessEvidence {
                                            cgroup_name: evidence.cgroup_name,
                                            pid: evidence.pid,
                                            start_time_ticks: evidence.start_time_ticks,
                                        }
                                    }),
                                },
                            );
                        }
                        "INSTANCE_TERMINATED" | "WATCHDOG_REAPED" => {
                            runtime_processes.remove(&instance_name);
                            if let (Some(lease_name), Some(fence_token)) =
                                (record.lease_name.as_ref(), record.fence_token)
                            {
                                runtime_processes.retain(|_, process| {
                                    process.lease_name.as_deref() != Some(lease_name)
                                        || process.fence_token != Some(fence_token)
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(RecoveryState {
            node_epoch,
            next_fence_token: max_fence_token.saturating_add(1).max(1),
            runtime_processes: runtime_processes.into_values().collect(),
        })
    }

    fn append_persisted(&self, record: PersistedRuntimeRecord) -> std::io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| std::io::Error::other("runtime journal lock poisoned"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &record).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_data()
    }

    fn semantic_events_for_source(
        &self,
        source: &semantic::Identity,
        namespace: &str,
    ) -> Result<Vec<DurableEventRecord>, ProviderError> {
        // A replay must observe whole appends: an append writes the JSON value,
        // newline and sync as one lock-held operation.
        let _guard = self.write_lock.lock().map_err(|_| {
            ProviderError::new(
                "runtime-journal",
                "JOURNAL_READ_FAILED",
                "runtime journal lock poisoned",
            )
        })?;
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let contents = std::fs::read_to_string(&self.path).map_err(|error| {
            ProviderError::new("runtime-journal", "JOURNAL_READ_FAILED", &error.to_string())
        })?;
        let terminal_newline = contents.ends_with('\n');
        let lines = contents.lines().collect::<Vec<_>>();
        let mut events = Vec::new();
        let mut previous_sequences = std::collections::BTreeMap::new();
        for (index, line) in lines.iter().enumerate() {
            // Match recovery: a torn final write was never durable, while every
            // earlier malformed record invalidates the replay source.
            let record = match serde_json::from_str::<PersistedRuntimeRecord>(line) {
                Ok(record) => record,
                Err(_) if index + 1 == lines.len() && !terminal_newline => continue,
                Err(error) => {
                    return Err(ProviderError::new(
                        "runtime-journal",
                        "JOURNAL_CORRUPT",
                        &error.to_string(),
                    ));
                }
            };
            let PersistedRuntimeRecord {
                event: record_kind,
                node_id,
                node_epoch,
                semantic_event,
                ..
            } = record;
            let event = match (record_kind.as_str(), semantic_event) {
                ("SEMANTIC_EVENT", Some(event)) => event,
                ("SEMANTIC_EVENT", None) | (_, Some(_)) => {
                    return Err(ProviderError::new(
                        "runtime-journal",
                        "JOURNAL_CORRUPT",
                        "semantic event record has inconsistent record fields",
                    ));
                }
                (_, None) => continue,
            };
            NamespaceId::new(event.namespace.as_str()).map_err(|error| {
                ProviderError::new("runtime-journal", "JOURNAL_CORRUPT", &error.message)
            })?;
            let event = DurableEventRecord {
                namespace: event.namespace,
                event: semantic::Event {
                    sequence: event.sequence,
                    source: semantic::Identity {
                        id: event.source_id,
                        generation: event.source_generation,
                    },
                    subject: semantic::Identity {
                        id: event.subject_id,
                        generation: event.subject_generation,
                    },
                    kind: event.kind,
                    observed_at_unix_ms: event.observed_at_unix_ms,
                    schema_id: event.schema_id,
                    body: event.body,
                },
            };
            event.event.validate().map_err(|error| {
                ProviderError::new("runtime-journal", "JOURNAL_CORRUPT", &error.message)
            })?;
            if event.event.source.id != node_id || event.event.source.generation != node_epoch {
                return Err(ProviderError::new(
                    "runtime-journal",
                    "JOURNAL_CORRUPT",
                    "semantic event source does not match its journal record",
                ));
            }
            let key = (
                event.namespace.clone(),
                event.event.source.id.clone(),
                event.event.source.generation,
            );
            if previous_sequences
                .insert(key, event.event.sequence)
                .is_some_and(|previous| event.event.sequence <= previous)
            {
                return Err(ProviderError::new(
                    "runtime-journal",
                    "JOURNAL_CORRUPT",
                    "semantic event sequence is not strictly ordered",
                ));
            }
            if event.namespace == namespace && event.event.source == *source {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn classify_recovery(
        recovery: &RecoveryState,
        observed: &[RuntimeProcessEvidence],
    ) -> Vec<RecoveryCandidate> {
        let mut matched = vec![false; recovery.runtime_processes.len()];
        let mut candidates = Vec::with_capacity(observed.len() + recovery.runtime_processes.len());
        for current in observed {
            let matching = recovery
                .runtime_processes
                .iter()
                .enumerate()
                .find(|(index, record)| {
                    !matched[*index]
                        && record
                            .runtime_evidence
                            .as_ref()
                            .is_some_and(|evidence| evidence == current)
                });
            if let Some((index, record)) = matching {
                matched[index] = true;
                candidates.push(RecoveryCandidate {
                    classification: if record.node_epoch < recovery.node_epoch {
                        RecoveryClassification::Stale
                    } else {
                        RecoveryClassification::Valid
                    },
                    record: Some(record.clone()),
                    observed: Some(current.clone()),
                });
            } else {
                candidates.push(RecoveryCandidate {
                    classification: RecoveryClassification::Foreign,
                    record: None,
                    observed: Some(current.clone()),
                });
            }
        }
        for (index, record) in recovery.runtime_processes.iter().enumerate() {
            if !matched[index] {
                candidates.push(RecoveryCandidate {
                    classification: RecoveryClassification::Unknown,
                    record: Some(record.clone()),
                    observed: None,
                });
            }
        }
        candidates
    }

    /// Completes the local-only restart boundary before any listener can make
    /// fresh allocations. Unknown and foreign processes are never adopted or
    /// killed; they instead keep startup fail-closed for operator action.
    pub fn recover_before_listeners(
        &self,
        node_id: &str,
        recovery: &RecoveryState,
        sandbox: &dyn SandboxBackend,
    ) -> Result<(), ProviderError> {
        let observed = sandbox.discover_recovery_processes()?;
        let candidates = Self::classify_recovery(recovery, &observed);
        let mut blocked = None;
        for candidate in &candidates {
            match candidate.classification {
                RecoveryClassification::Stale => {
                    let record = candidate
                        .record
                        .as_ref()
                        .expect("stale candidate has journal record");
                    let evidence = candidate
                        .observed
                        .as_ref()
                        .expect("stale candidate has verified sandbox evidence");
                    let cleanup = sandbox.recover_stale_process(evidence)?;
                    let event = if cleanup.complete {
                        RuntimeJournalEvent::InstanceTerminated
                    } else {
                        RuntimeJournalEvent::InstanceCleanupFailed
                    };
                    self.append(RuntimeJournalRecord {
                        event,
                        node_id: node_id.to_string(),
                        node_epoch: recovery.node_epoch,
                        instance_name: Some(record.instance_name.clone()),
                        lease_name: record.lease_name.clone(),
                        fence_token: record.fence_token,
                        reason_code: cleanup.reason_code.clone(),
                        runtime_evidence: None,
                    })?;
                    if !cleanup.complete {
                        return Err(ProviderError::new(
                            "runtime-journal",
                            "RECOVERY_CLEANUP_INCOMPLETE",
                            &cleanup.reason_code,
                        ));
                    }
                }
                RecoveryClassification::Valid => {
                    blocked = Some("RECOVERY_VALID_PROCESS_UNADOPTED");
                }
                RecoveryClassification::Unknown => {
                    blocked = Some("RECOVERY_UNKNOWN_PROCESS");
                }
                RecoveryClassification::Foreign => {
                    blocked = Some("RECOVERY_FOREIGN_PROCESS");
                }
            }
        }
        if let Some(reason_code) = blocked {
            return Err(ProviderError::new(
                "runtime-journal",
                reason_code,
                "restart recovery refuses to adopt or terminate unverified runtime state",
            ));
        }
        Ok(())
    }
}

impl RuntimeJournalSink for FileRuntimeJournal {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        self.append_persisted(PersistedRuntimeRecord {
            observed_at_millis: now_millis(),
            event: event_name(record.event).to_string(),
            node_id: record.node_id,
            node_epoch: record.node_epoch,
            instance_name: record.instance_name,
            lease_name: record.lease_name,
            fence_token: record.fence_token,
            reason_code: record.reason_code,
            runtime_evidence: record.runtime_evidence.map(|evidence| {
                PersistedRuntimeProcessEvidence {
                    cgroup_name: evidence.cgroup_name,
                    pid: evidence.pid,
                    start_time_ticks: evidence.start_time_ticks,
                }
            }),
            semantic_event: None,
        })
        .map_err(|error| {
            ProviderError::new(
                "runtime-journal",
                "JOURNAL_WRITE_FAILED",
                &error.to_string(),
            )
        })
    }
}

impl DurableEventStore for FileRuntimeJournal {
    fn append_event(&self, record: DurableEventRecord) -> Result<(), ProviderError> {
        NamespaceId::new(record.namespace.as_str()).map_err(|error| {
            ProviderError::new("runtime-journal", "EVENT_WRITE_INVALID", &error.message)
        })?;
        record.event.validate().map_err(|error| {
            ProviderError::new("runtime-journal", "EVENT_WRITE_INVALID", &error.message)
        })?;
        let event = record.event;
        self.append_persisted(PersistedRuntimeRecord {
            observed_at_millis: now_millis(),
            event: "SEMANTIC_EVENT".to_string(),
            node_id: event.source.id.clone(),
            node_epoch: event.source.generation,
            instance_name: None,
            lease_name: None,
            fence_token: None,
            reason_code: "EVENT_PUBLISHED".to_string(),
            runtime_evidence: None,
            semantic_event: Some(PersistedSemanticEvent {
                namespace: record.namespace,
                sequence: event.sequence,
                source_id: event.source.id,
                source_generation: event.source.generation,
                subject_id: event.subject.id,
                subject_generation: event.subject.generation,
                kind: event.kind,
                observed_at_unix_ms: event.observed_at_unix_ms,
                schema_id: event.schema_id,
                body: event.body,
            }),
        })
        .map_err(|error| {
            ProviderError::new("runtime-journal", "EVENT_WRITE_FAILED", &error.to_string())
        })
    }

    fn events_for_source(
        &self,
        source: &semantic::Identity,
        namespace: &str,
    ) -> Result<Option<Vec<DurableEventRecord>>, ProviderError> {
        Ok(Some(self.semantic_events_for_source(source, namespace)?))
    }
}

fn event_name(event: RuntimeJournalEvent) -> &'static str {
    match event {
        RuntimeJournalEvent::KernelStarted => "KERNEL_STARTED",
        RuntimeJournalEvent::LeaseAcquired => "LEASE_ACQUIRED",
        RuntimeJournalEvent::LeaseReleaseStarted => "LEASE_RELEASE_STARTED",
        RuntimeJournalEvent::LeaseReleased => "LEASE_RELEASED",
        RuntimeJournalEvent::LeaseRevoked => "LEASE_REVOKED",
        RuntimeJournalEvent::FenceAdvanced => "FENCE_ADVANCED",
        RuntimeJournalEvent::InstanceLaunching => "INSTANCE_LAUNCHING",
        RuntimeJournalEvent::InstanceLaunched => "INSTANCE_LAUNCHED",
        RuntimeJournalEvent::InstanceTerminated => "INSTANCE_TERMINATED",
        RuntimeJournalEvent::InstanceCleanupFailed => "INSTANCE_CLEANUP_FAILED",
        RuntimeJournalEvent::WatchdogReaped => "WATCHDOG_REAPED",
        RuntimeJournalEvent::WorkerLost => "WORKER_LOST",
        RuntimeJournalEvent::OperationLost => "OPERATION_LOST",
        RuntimeJournalEvent::EndpointRevoked => "ENDPOINT_REVOKED",
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests;
