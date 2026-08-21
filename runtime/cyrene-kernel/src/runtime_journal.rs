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
        RuntimeJournalEvent::LeaseReserved => "LEASE_RESERVED",
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
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecoverySandbox {
        observed: Vec<RuntimeProcessEvidence>,
        recovered: Mutex<Vec<RuntimeProcessEvidence>>,
    }

    impl cy_kernel_api::ProcessRuntime for RecoverySandbox {
        fn preflight(&self) -> cy_kernel_api::NodeCapabilities {
            cy_kernel_api::NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            }
        }

        fn launch(
            &self,
            _plan: &cy_kernel_api::LaunchPlan,
            _binding: &cy_kernel_api::DeviceBinding,
        ) -> Result<cy_kernel_api::ProcessHandle, ProviderError> {
            Err(ProviderError::new("test", "UNUSED", "recovery test"))
        }

        fn stop(
            &self,
            _handle: &cy_kernel_api::ProcessHandle,
            _request: &cy_kernel_api::StopRequest,
        ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
            Err(ProviderError::new("test", "UNUSED", "recovery test"))
        }
    }

    impl SandboxBackend for RecoverySandbox {
        fn backend_id(&self) -> &str {
            "recovery-test"
        }

        fn discover_recovery_processes(
            &self,
        ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
            Ok(self.observed.clone())
        }

        fn recover_stale_process(
            &self,
            evidence: &RuntimeProcessEvidence,
        ) -> Result<cy_kernel_api::CleanupReport, ProviderError> {
            self.recovered.lock().unwrap().push(evidence.clone());
            Ok(cy_kernel_api::CleanupReport {
                complete: true,
                exit_code: None,
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "RECOVERY_CLEANUP_COMPLETE".to_string(),
            })
        }
    }

    fn process_record(
        instance_name: &str,
        epoch: u64,
        evidence: Option<RuntimeProcessEvidence>,
    ) -> RuntimeProcessRecord {
        RuntimeProcessRecord {
            instance_name: instance_name.to_string(),
            lease_name: Some(format!("lease-{instance_name}")),
            fence_token: Some(epoch),
            node_epoch: epoch,
            runtime_evidence: evidence,
        }
    }

    #[test]
    fn restart_advances_epoch_and_fence_without_recovering_instances() {
        let directory = tempfile::tempdir().unwrap();
        let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
        let first = journal.begin_epoch("node-1").unwrap();
        journal
            .append(RuntimeJournalRecord {
                event: RuntimeJournalEvent::InstanceLaunched,
                node_id: "node-1".to_string(),
                node_epoch: first.node_epoch,
                instance_name: Some("instance-1".to_string()),
                lease_name: Some("lease-1".to_string()),
                fence_token: Some(41),
                reason_code: "WORKER_LAUNCHED".to_string(),
                runtime_evidence: Some(RuntimeProcessEvidence {
                    cgroup_name: "instance-1".to_string(),
                    pid: 42,
                    start_time_ticks: 1,
                }),
            })
            .unwrap();
        let next = journal.begin_epoch("node-1").unwrap();
        assert!(next.node_epoch > first.node_epoch);
        assert_eq!(next.next_fence_token, 42);
        assert_eq!(next.runtime_processes.len(), 1);
        assert_eq!(next.runtime_processes[0].instance_name, "instance-1");
    }

    #[test]
    fn recovery_classifies_without_adopting_valid_unknown_or_foreign_processes() {
        let valid = RuntimeProcessEvidence {
            cgroup_name: "instance-valid".to_string(),
            pid: 11,
            start_time_ticks: 101,
        };
        let stale = RuntimeProcessEvidence {
            cgroup_name: "instance-stale".to_string(),
            pid: 12,
            start_time_ticks: 102,
        };
        let foreign = RuntimeProcessEvidence {
            cgroup_name: "instance-foreign".to_string(),
            pid: 13,
            start_time_ticks: 103,
        };
        let recovery = RecoveryState {
            node_epoch: 9,
            next_fence_token: 1,
            runtime_processes: vec![
                process_record("valid", 9, Some(valid.clone())),
                process_record("stale", 8, Some(stale.clone())),
                process_record("unknown", 8, None),
            ],
        };
        let candidates = FileRuntimeJournal::classify_recovery(&recovery, &[valid, stale, foreign]);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.classification)
                .collect::<Vec<_>>(),
            vec![
                RecoveryClassification::Valid,
                RecoveryClassification::Stale,
                RecoveryClassification::Foreign,
                RecoveryClassification::Unknown,
            ]
        );
    }

    #[test]
    fn recovery_reaps_only_exact_stale_evidence_and_journals_terminal_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
        let first = journal.begin_epoch("node-1").unwrap();
        let evidence = RuntimeProcessEvidence {
            cgroup_name: "instance-stale".to_string(),
            pid: 42,
            start_time_ticks: 7,
        };
        journal
            .append(RuntimeJournalRecord {
                event: RuntimeJournalEvent::InstanceLaunched,
                node_id: "node-1".to_string(),
                node_epoch: first.node_epoch,
                instance_name: Some("worker-stale".to_string()),
                lease_name: Some("lease-stale".to_string()),
                fence_token: Some(5),
                reason_code: "WORKER_LAUNCHED".to_string(),
                runtime_evidence: Some(evidence.clone()),
            })
            .unwrap();
        let recovery = journal.begin_epoch("node-1").unwrap();
        let sandbox = RecoverySandbox {
            observed: vec![evidence.clone()],
            recovered: Mutex::new(Vec::new()),
        };

        journal
            .recover_before_listeners("node-1", &recovery, &sandbox)
            .unwrap();

        assert_eq!(*sandbox.recovered.lock().unwrap(), vec![evidence]);
        assert!(journal
            .recover("node-1")
            .unwrap()
            .runtime_processes
            .is_empty());
    }

    #[test]
    fn recovery_never_adopts_a_valid_process() {
        let directory = tempfile::tempdir().unwrap();
        let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
        let evidence = RuntimeProcessEvidence {
            cgroup_name: "instance-current".to_string(),
            pid: 42,
            start_time_ticks: 7,
        };
        let recovery = RecoveryState {
            node_epoch: 4,
            next_fence_token: 1,
            runtime_processes: vec![process_record("worker-current", 4, Some(evidence.clone()))],
        };
        let sandbox = RecoverySandbox {
            observed: vec![evidence],
            recovered: Mutex::new(Vec::new()),
        };

        let error = journal
            .recover_before_listeners("node-1", &recovery, &sandbox)
            .unwrap_err();
        assert_eq!(error.reason_code, "RECOVERY_VALID_PROCESS_UNADOPTED");
        assert!(sandbox.recovered.lock().unwrap().is_empty());
    }

    #[test]
    fn recovery_ignores_only_an_incomplete_final_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.jsonl");
        let journal = FileRuntimeJournal::open(&path).unwrap();
        let first = journal.begin_epoch("node-1").unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"partial\"")
            .unwrap();

        let next = journal.begin_epoch("node-1").unwrap();
        assert!(next.node_epoch > first.node_epoch);
    }

    #[test]
    fn recovery_rejects_nonfinal_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.jsonl");
        std::fs::write(&path, "{\"partial\"\n{\"also_partial\"").unwrap();
        let journal = FileRuntimeJournal::open(&path).unwrap();

        assert_eq!(
            journal.begin_epoch("node-1").unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    /// Crash/restart must not reuse fence tokens. The fence floor is taken from
    /// the durably persisted journal (`recover()` returns max historical fence
    /// + 1); a fresh manager seeded with that floor must allocate a strictly
    ///   greater token than the lease that existed before the restart.
    #[test]
    fn crash_restart_does_not_reuse_fence_tokens() {
        use cy_kernel_api::{
            semantic::{
                Capability, CapabilityRequirement, Identity, Resource, ResourceQuery, ResourceState,
            },
            ResourceLeaseManager, ResourceRequest, RuntimeJournalEvent,
        };
        use cy_resource_manager::InMemoryResourceManager;

        let directory = tempfile::tempdir().unwrap();
        let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();

        let resource = Resource {
            identity: Identity {
                id: "resource-1".to_string(),
                generation: 1,
            },
            provider: Identity {
                id: "test-provider".to_string(),
                generation: 1,
            },
            resource_class: "accelerator".to_string(),
            capabilities: vec![Capability {
                id: "accelerator.compute".to_string(),
                revision: 1,
                properties: Default::default(),
            }],
            capacity: Default::default(),
            attributes: Default::default(),
            state: ResourceState::Ready,
            reason_code: "test-ready".to_string(),
            summary: "healthy".to_string(),
            links: Vec::new(),
        };

        let generation = 1_u64;
        let request = ResourceRequest {
            lease_name: "lease-before-restart".to_string(),
            expected_inventory_generation: generation,
            holder: Identity {
                id: "worker/test".to_string(),
                generation: 1,
            },
            query: ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: vec![CapabilityRequirement {
                    id: "accelerator.compute".to_string(),
                    minimum_revision: 1,
                    required_properties: Default::default(),
                }],
                minimum_capacity: Default::default(),
            },
            expires_at_unix_ms: None,
            limits: Default::default(),
        };

        // Pre-restart: acquire a lease with fence token N and durably record it.
        let manager_before = InMemoryResourceManager::new("node-1", vec![resource.clone()]);
        let lease_before = manager_before.reserve(request.clone()).unwrap();
        let fence_before = lease_before.fence_token;
        journal
            .append(RuntimeJournalRecord {
                event: RuntimeJournalEvent::LeaseReserved,
                node_id: "node-1".to_string(),
                node_epoch: 0,
                instance_name: None,
                lease_name: Some(lease_before.name.clone()),
                fence_token: Some(fence_before),
                reason_code: "LEASE_RESERVED".to_string(),
                runtime_evidence: None,
            })
            .unwrap();
        drop(manager_before);

        // Restart: recover the durable fence floor and seed a fresh manager.
        let recovery = journal.recover("node-1").unwrap();
        assert_eq!(recovery.next_fence_token, fence_before + 1);
        let manager_after = InMemoryResourceManager::with_next_fence_token(
            "node-1",
            vec![resource],
            recovery.next_fence_token,
        );
        let lease_after = manager_after.reserve(request).unwrap();
        assert!(
            lease_after.fence_token > fence_before,
            "fence token must not be reused across a restart"
        );
    }

    #[test]
    fn two_epoch_restart_requires_client_resnapshot_and_never_adopts_old_authority() {
        use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

        use cy_kernel_api::{
            semantic, AuthorityCallContext, CleanupReport, DeviceBinding, EnforcementMode,
            HealthReport, HostInventoryProvider, InstalledPluginResolver, InventorySnapshot,
            KernelAuthority, LaunchPlan, NodeCapabilities, ProcessHandle, ProcessRuntime,
            ResolvedLaunchPlan, ResourceProvider, SandboxBackend, StopRequest,
            VerifiedInstallation,
        };
        use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter};
        use cy_resource_manager::InMemoryResourceManager;

        #[derive(Clone)]
        struct TestHardware {
            resource: semantic::Resource,
        }

        impl HostInventoryProvider for TestHardware {
            fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
                Ok(InventorySnapshot {
                    generation: 1,
                    resources: vec![self.resource.clone()],
                    capabilities: NodeCapabilities {
                        ready: true,
                        facts: Vec::new(),
                        enforcement: Vec::new(),
                    },
                })
            }
        }

        impl ResourceProvider for TestHardware {
            fn adapter_id(&self) -> &str {
                "restart-test-hardware"
            }

            fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
                Ok(vec![self.resource.clone()])
            }

            fn create_binding(
                &self,
                resource: &semantic::Resource,
            ) -> Result<DeviceBinding, ProviderError> {
                Ok(DeviceBinding {
                    resource_id: resource.identity.id.clone(),
                    nodes: Vec::new(),
                    environment: BTreeMap::new(),
                    required_gids: Vec::new(),
                    enforcement: EnforcementMode::ObserveOnly,
                    adapter_id: self.adapter_id().to_string(),
                    reason_code: "RESTART_TEST_BINDING".to_string(),
                })
            }

            fn read_health(&self, _resource_id: &str) -> Result<HealthReport, ProviderError> {
                Ok(HealthReport {
                    healthy: Some(true),
                    reason_code: "READY".to_string(),
                    summary: "restart test resource is ready".to_string(),
                })
            }
        }

        struct TestWorkerResolver;

        impl InstalledPluginResolver for TestWorkerResolver {
            fn resolve_launch_plan(
                &self,
                _installation: &VerifiedInstallation,
                _instance_name: &str,
            ) -> Result<ResolvedLaunchPlan, ProviderError> {
                Err(ProviderError::new(
                    "restart-test",
                    "UNUSED",
                    "legacy launch is unused",
                ))
            }

            fn resolve_worker_launch_plan(
                &self,
                worker: &semantic::Worker,
            ) -> Result<ResolvedLaunchPlan, ProviderError> {
                Ok(ResolvedLaunchPlan {
                    installation: VerifiedInstallation {
                        installation_name: "restart-test".to_string(),
                        manifest_digest: "sha256:restart-test".to_string(),
                        artifact_digest: "sha256:restart-test".to_string(),
                        verified_signature_identity: "restart-test".to_string(),
                    },
                    plan: LaunchPlan {
                        instance_name: worker.identity.id.clone(),
                        executable: PathBuf::from("restart-test-worker"),
                        args: Vec::new(),
                        environment: BTreeMap::new(),
                        cgroup_name: format!("worker-{}", worker.identity.id),
                        limits: Default::default(),
                        transport_socket: None,
                    },
                })
            }
        }

        struct NoAdoptionRuntime;

        impl ProcessRuntime for NoAdoptionRuntime {
            fn preflight(&self) -> NodeCapabilities {
                NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                }
            }

            fn launch(
                &self,
                plan: &LaunchPlan,
                _binding: &DeviceBinding,
            ) -> Result<ProcessHandle, ProviderError> {
                Ok(ProcessHandle {
                    pid: 7,
                    cgroup_path: PathBuf::from(format!("/restart-test/{}", plan.cgroup_name)),
                    start_time_ticks: Some(11),
                    transport_socket: None,
                })
            }

            fn stop(
                &self,
                _handle: &ProcessHandle,
                _request: &StopRequest,
            ) -> Result<CleanupReport, ProviderError> {
                Ok(CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "STOPPED".to_string(),
                })
            }
        }

        impl SandboxBackend for NoAdoptionRuntime {
            fn backend_id(&self) -> &str {
                "restart-test"
            }
            // Recovery uses SandboxBackend's fail-closed defaults: this runtime
            // has no stale-process discovery or adoption path.
        }

        struct StaleCleanupRuntime {
            observed: Vec<RuntimeProcessEvidence>,
            reaped: Mutex<Vec<RuntimeProcessEvidence>>,
        }

        impl ProcessRuntime for StaleCleanupRuntime {
            fn preflight(&self) -> NodeCapabilities {
                NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                }
            }

            fn launch(
                &self,
                _plan: &LaunchPlan,
                _binding: &DeviceBinding,
            ) -> Result<ProcessHandle, ProviderError> {
                Err(ProviderError::new("restart-test", "UNUSED", "cleanup only"))
            }

            fn stop(
                &self,
                _handle: &ProcessHandle,
                _request: &StopRequest,
            ) -> Result<CleanupReport, ProviderError> {
                Err(ProviderError::new("restart-test", "UNUSED", "cleanup only"))
            }
        }

        impl SandboxBackend for StaleCleanupRuntime {
            fn backend_id(&self) -> &str {
                "restart-test-cleanup"
            }

            fn discover_recovery_processes(
                &self,
            ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
                Ok(self.observed.clone())
            }

            fn recover_stale_process(
                &self,
                evidence: &RuntimeProcessEvidence,
            ) -> Result<CleanupReport, ProviderError> {
                if !self.observed.contains(evidence) {
                    return Err(ProviderError::new(
                        "restart-test",
                        "RECOVERY_EVIDENCE_UNKNOWN",
                        "cleanup requires exact observed evidence",
                    ));
                }
                self.reaped.lock().unwrap().push(evidence.clone());
                Ok(CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "STALE_PROCESS_REAPED".to_string(),
                })
            }
        }

        let resource = semantic::Resource {
            identity: semantic::Identity {
                id: "resource-restart".to_string(),
                generation: 1,
            },
            provider: semantic::Identity {
                id: "restart-test-hardware".to_string(),
                generation: 1,
            },
            resource_class: "accelerator".to_string(),
            capabilities: vec![semantic::Capability {
                id: "accelerator.compute".to_string(),
                revision: 1,
                properties: BTreeMap::new(),
            }],
            capacity: BTreeMap::new(),
            attributes: BTreeMap::new(),
            state: semantic::ResourceState::Ready,
            reason_code: "READY".to_string(),
            summary: "restart test resource".to_string(),
            links: Vec::new(),
        };
        let query = semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        };
        let principal = semantic::Principal {
            identity: semantic::Identity {
                id: "client-restart-golden".to_string(),
                generation: 1,
            },
        };
        let context = |request_id: &str| AuthorityCallContext {
            contract: semantic::ContractRevision::current(),
            namespace: NamespaceId::default(),
            request_id: request_id.to_string(),
            idempotency_key: request_id.to_string(),
        };
        let expiry = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + Duration::from_secs(60).as_millis() as u64
        };

        let directory = tempfile::tempdir().unwrap();
        let journal =
            Arc::new(FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap());

        // Epoch N owns a lease, Worker, Operation, and Endpoint and persists
        // source-scoped events through the same FileRuntimeJournal.
        let epoch_n = journal.begin_epoch("node-restart").unwrap();
        let hardware_n = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let resources_n = Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-restart",
            vec![resource.clone()],
            epoch_n.next_fence_token,
        ));
        let adapter_n = KernelServiceAdapter::new(
            Arc::new(KernelDaemon::new(
                hardware_n.clone(),
                hardware_n,
                resources_n,
                Arc::new(NoAdoptionRuntime),
                "node-restart",
                epoch_n.node_epoch,
            )),
            Arc::new(TestWorkerResolver),
        )
        .with_runtime_journal(journal.clone())
        .with_event_store(journal.clone());
        let authority_n = adapter_n.authority();
        let worker = semantic::Identity {
            id: "worker-before-restart".to_string(),
            generation: 1,
        };
        let lease_n = authority_n
            .acquire_lease(
                &context("lease-before-restart"),
                &principal,
                worker.clone(),
                query.clone(),
                expiry(),
            )
            .unwrap();
        authority_n
            .start_worker(
                &context("worker-before-restart"),
                &principal,
                semantic::Worker {
                    identity: worker.clone(),
                    principal: principal.identity.clone(),
                    provider: resource.provider.clone(),
                    lease: lease_n.identity.clone(),
                    state: semantic::WorkerState::Registered,
                    execution_ref: "opaque-restart-test".to_string(),
                    limits: BTreeMap::new(),
                },
            )
            .unwrap();
        let operation_n = semantic::Operation {
            identity: semantic::Identity {
                id: "operation-before-restart".to_string(),
                generation: 1,
            },
            owner: principal.identity.clone(),
            executor: worker.clone(),
            kind: "worker.invoke".to_string(),
            state: semantic::OperationState::Created,
            deadline_unix_ms: None,
            parent: None,
            metadata: BTreeMap::new(),
        };
        authority_n
            .create_operation(
                &context("operation-before-restart"),
                &principal,
                operation_n.clone(),
            )
            .unwrap();
        let endpoint_n = semantic::Endpoint {
            identity: semantic::Identity {
                id: "endpoint-before-restart".to_string(),
                generation: 1,
            },
            provider: resource.provider.clone(),
            owner: worker.clone(),
            transport: "transport.uds".to_string(),
            schema_id: "cyrene.endpoint.v1".to_string(),
            capabilities: Vec::new(),
            public_attributes: BTreeMap::new(),
        };
        authority_n
            .publish_endpoint(
                &context("endpoint-before-restart"),
                &principal,
                endpoint_n.clone(),
            )
            .unwrap();
        let snapshot_n = authority_n
            .snapshot(&context("snapshot-before-restart"), &principal)
            .unwrap();
        assert!(
            snapshot_n.cursor.sequence > 0,
            "epoch N must expose a durable cursor"
        );
        assert!(snapshot_n.leases.contains(&lease_n));
        assert!(snapshot_n.operations.contains(&operation_n));
        assert!(snapshot_n
            .workers
            .iter()
            .any(|current| current.identity == worker));
        assert!(snapshot_n.endpoints.contains(&endpoint_n));
        let cursor_n = snapshot_n.cursor.clone();
        let fence_n = lease_n.fence_token;
        drop(authority_n);
        drop(adapter_n);

        // Epoch N+1 closes the exact stale process, then starts from a fresh
        // resource ledger and authority. It never restores any old authority.
        let epoch_n_plus_one = journal.begin_epoch("node-restart").unwrap();
        assert!(epoch_n_plus_one.node_epoch > epoch_n.node_epoch);
        assert_eq!(epoch_n_plus_one.next_fence_token, fence_n + 1);
        let stale_evidence = epoch_n_plus_one.runtime_processes[0]
            .runtime_evidence
            .clone()
            .unwrap();
        let cleanup_runtime = StaleCleanupRuntime {
            observed: vec![stale_evidence.clone()],
            reaped: Mutex::new(Vec::new()),
        };
        journal
            .recover_before_listeners("node-restart", &epoch_n_plus_one, &cleanup_runtime)
            .unwrap();
        assert_eq!(
            *cleanup_runtime.reaped.lock().unwrap(),
            vec![stale_evidence]
        );

        let hardware_n_plus_one = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let resources_n_plus_one = Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-restart",
            vec![resource.clone()],
            epoch_n_plus_one.next_fence_token,
        ));
        let authority_n_plus_one = KernelServiceAdapter::new(
            Arc::new(KernelDaemon::new(
                hardware_n_plus_one.clone(),
                hardware_n_plus_one,
                resources_n_plus_one,
                Arc::new(NoAdoptionRuntime),
                "node-restart",
                epoch_n_plus_one.node_epoch,
            )),
            Arc::new(TestWorkerResolver),
        )
        .with_runtime_journal(journal.clone())
        .with_event_store(journal)
        .authority();

        // The same client binds anew in epoch N+1 before asking to replay its
        // old cursor; binding does not resurrect its prior authority objects.
        let operation_n_plus_one = semantic::Operation {
            identity: semantic::Identity {
                id: "operation-after-restart".to_string(),
                generation: 1,
            },
            owner: principal.identity.clone(),
            executor: semantic::Identity {
                id: "worker-after-restart".to_string(),
                generation: 1,
            },
            kind: "worker.invoke".to_string(),
            state: semantic::OperationState::Created,
            deadline_unix_ms: None,
            parent: None,
            metadata: BTreeMap::new(),
        };
        authority_n_plus_one
            .create_operation(
                &context("bind-after-restart"),
                &principal,
                operation_n_plus_one.clone(),
            )
            .unwrap();
        let source_changed = authority_n_plus_one
            .events_after(&context("replay-after-restart"), &principal, &cursor_n, 256)
            .unwrap();
        assert_eq!(source_changed.status, semantic::ReplayStatus::SourceChanged);
        assert!(source_changed.events.is_empty());

        let lease_n_plus_one = authority_n_plus_one
            .acquire_lease(
                &context("lease-after-restart"),
                &principal,
                operation_n_plus_one.executor.clone(),
                query,
                expiry(),
            )
            .unwrap();
        assert!(lease_n_plus_one.fence_token > fence_n);
        let snapshot_n_plus_one = authority_n_plus_one
            .snapshot(&context("snapshot-after-restart"), &principal)
            .unwrap();
        assert_ne!(snapshot_n_plus_one.source, snapshot_n.source);
        assert_eq!(snapshot_n_plus_one.source, source_changed.source);
        assert_eq!(
            snapshot_n_plus_one.cursor.source,
            snapshot_n_plus_one.source
        );
        assert!(snapshot_n_plus_one.cursor.sequence > 0);
        assert_eq!(snapshot_n_plus_one.leases, vec![lease_n_plus_one]);
        assert_eq!(snapshot_n_plus_one.operations, vec![operation_n_plus_one]);
        assert!(snapshot_n_plus_one.workers.is_empty());
        assert!(snapshot_n_plus_one.endpoints.is_empty());
        assert!(authority_n_plus_one
            .events_after(
                &context("resume-after-resnapshot"),
                &principal,
                &snapshot_n_plus_one.cursor,
                256,
            )
            .unwrap()
            .events
            .is_empty());
    }

    #[test]
    fn semantic_events_are_durable_and_source_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let journal = FileRuntimeJournal::open(directory.path().join("runtime.jsonl")).unwrap();
        let source = semantic::Identity {
            id: "kernel/node-1".to_string(),
            generation: 7,
        };
        journal
            .append_event(DurableEventRecord {
                namespace: "default".to_string(),
                event: semantic::Event {
                    sequence: 3,
                    source: source.clone(),
                    subject: semantic::Identity {
                        id: "worker-1".to_string(),
                        generation: 1,
                    },
                    kind: "worker.lost".to_string(),
                    observed_at_unix_ms: 1,
                    schema_id: "cyrene.worker.v1".to_string(),
                    body: Vec::new(),
                },
            })
            .unwrap();
        let records = journal
            .events_for_source(&source, "default")
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.sequence, 3);
        assert_eq!(records[0].event.kind, "worker.lost");
        assert!(journal
            .events_for_source(
                &semantic::Identity {
                    id: source.id.clone(),
                    generation: 8,
                },
                "default",
            )
            .unwrap()
            .unwrap()
            .is_empty());

        let mut out_of_order = records[0].clone();
        out_of_order.event.sequence = 2;
        journal.append_event(out_of_order).unwrap();
        assert_eq!(
            journal
                .events_for_source(&source, "default")
                .unwrap_err()
                .reason_code,
            "JOURNAL_CORRUPT"
        );
    }

    #[test]
    fn semantic_event_replay_ignores_an_incomplete_final_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.jsonl");
        let journal = FileRuntimeJournal::open(&path).unwrap();
        let source = semantic::Identity {
            id: "kernel/node-1".to_string(),
            generation: 7,
        };
        journal
            .append_event(DurableEventRecord {
                namespace: "default".to_string(),
                event: semantic::Event {
                    sequence: 1,
                    source: source.clone(),
                    subject: semantic::Identity {
                        id: "worker-1".to_string(),
                        generation: 1,
                    },
                    kind: "worker.lost".to_string(),
                    observed_at_unix_ms: 1,
                    schema_id: "cyrene.worker.v1".to_string(),
                    body: Vec::new(),
                },
            })
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{\"partial\"")
            .unwrap();

        let records = journal
            .events_for_source(&source, "default")
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.sequence, 1);
    }

    /// Golden Test C — Restart With Running Worker
    ///
    /// Scenario:
    /// Start real Worker + Lease + Endpoint in Epoch N.
    /// Kill Kernel unexpectedly (crash simulation).
    /// Restart in Epoch N+1.
    ///
    /// Verifies:
    /// - old authority is not silently adopted;
    /// - recovery classifies reality correctly;
    /// - stale process is reaped only with exact evidence;
    /// - foreign/unknown is not killed;
    /// - old cursor gets SOURCE_CHANGED;
    /// - fresh snapshot contains no stale authority;
    /// - replacement Fence is strictly newer.
    #[test]
    fn golden_test_c_restart_with_running_worker_no_adoption_and_fencing() {
        use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

        use cy_kernel_api::{
            semantic, AuthorityCallContext, CleanupReport, DeviceBinding, EnforcementMode,
            HealthReport, HostInventoryProvider, InstalledPluginResolver, InventorySnapshot,
            KernelAuthority, LaunchPlan, NodeCapabilities, ProcessHandle, ProcessRuntime,
            ResolvedLaunchPlan, ResourceProvider, SandboxBackend, StopRequest,
            VerifiedInstallation,
        };
        use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter};
        use cy_resource_manager::InMemoryResourceManager;

        #[derive(Clone)]
        struct TestHardware {
            resource: semantic::Resource,
        }

        impl HostInventoryProvider for TestHardware {
            fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
                Ok(InventorySnapshot {
                    generation: 1,
                    resources: vec![self.resource.clone()],
                    capabilities: NodeCapabilities {
                        ready: true,
                        facts: Vec::new(),
                        enforcement: Vec::new(),
                    },
                })
            }
        }

        impl ResourceProvider for TestHardware {
            fn adapter_id(&self) -> &str {
                "restart-hardware-c"
            }

            fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
                Ok(vec![self.resource.clone()])
            }

            fn create_binding(
                &self,
                resource: &semantic::Resource,
            ) -> Result<DeviceBinding, ProviderError> {
                Ok(DeviceBinding {
                    resource_id: resource.identity.id.clone(),
                    nodes: Vec::new(),
                    environment: BTreeMap::new(),
                    required_gids: Vec::new(),
                    enforcement: EnforcementMode::ObserveOnly,
                    adapter_id: self.adapter_id().to_string(),
                    reason_code: "GOLDEN_C_BINDING".to_string(),
                })
            }

            fn read_health(&self, _resource_id: &str) -> Result<HealthReport, ProviderError> {
                Ok(HealthReport {
                    healthy: Some(true),
                    reason_code: "READY".to_string(),
                    summary: "ready".to_string(),
                })
            }
        }

        struct TestWorkerResolver;

        impl InstalledPluginResolver for TestWorkerResolver {
            fn resolve_launch_plan(
                &self,
                _installation: &VerifiedInstallation,
                _instance_name: &str,
            ) -> Result<ResolvedLaunchPlan, ProviderError> {
                Err(ProviderError::new("golden-c", "UNUSED", "unused"))
            }

            fn resolve_worker_launch_plan(
                &self,
                worker: &semantic::Worker,
            ) -> Result<ResolvedLaunchPlan, ProviderError> {
                Ok(ResolvedLaunchPlan {
                    installation: VerifiedInstallation {
                        installation_name: "golden-c".to_string(),
                        manifest_digest: "sha256:golden-c".to_string(),
                        artifact_digest: "sha256:golden-c".to_string(),
                        verified_signature_identity: "golden-c".to_string(),
                    },
                    plan: LaunchPlan {
                        instance_name: worker.identity.id.clone(),
                        executable: PathBuf::from("worker"),
                        args: Vec::new(),
                        environment: BTreeMap::new(),
                        cgroup_name: format!("instance-{}", worker.identity.id),
                        limits: Default::default(),
                        transport_socket: None,
                    },
                })
            }
        }

        #[derive(Clone)]
        struct ControlledRecoverySandbox {
            observed: Arc<Mutex<Vec<RuntimeProcessEvidence>>>,
            reaped: Arc<Mutex<Vec<RuntimeProcessEvidence>>>,
        }

        impl ProcessRuntime for ControlledRecoverySandbox {
            fn preflight(&self) -> NodeCapabilities {
                NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                }
            }

            fn launch(
                &self,
                plan: &LaunchPlan,
                _binding: &DeviceBinding,
            ) -> Result<ProcessHandle, ProviderError> {
                let evidence = RuntimeProcessEvidence {
                    cgroup_name: plan.cgroup_name.clone(),
                    pid: 3030,
                    start_time_ticks: 5000,
                };
                self.observed.lock().unwrap().push(evidence);
                Ok(ProcessHandle {
                    pid: 3030,
                    cgroup_path: PathBuf::from(format!("/test/{}", plan.cgroup_name)),
                    start_time_ticks: Some(5000),
                    transport_socket: None,
                })
            }

            fn stop(
                &self,
                _handle: &ProcessHandle,
                _request: &StopRequest,
            ) -> Result<CleanupReport, ProviderError> {
                Ok(CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "CONTROLLED_STOP".to_string(),
                })
            }
        }

        impl SandboxBackend for ControlledRecoverySandbox {
            fn backend_id(&self) -> &str {
                "controlled-recovery-backend"
            }

            fn discover_recovery_processes(
                &self,
            ) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
                Ok(self.observed.lock().unwrap().clone())
            }

            fn recover_stale_process(
                &self,
                evidence: &RuntimeProcessEvidence,
            ) -> Result<CleanupReport, ProviderError> {
                self.reaped.lock().unwrap().push(evidence.clone());
                Ok(CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "STALE_PROCESS_REAPED".to_string(),
                })
            }
        }

        let resource = semantic::Resource {
            identity: semantic::Identity {
                id: "res-golden-c".to_string(),
                generation: 1,
            },
            provider: semantic::Identity {
                id: "restart-hardware-c".to_string(),
                generation: 1,
            },
            resource_class: "accelerator".to_string(),
            capabilities: vec![semantic::Capability {
                id: "accelerator.compute".to_string(),
                revision: 1,
                properties: BTreeMap::new(),
            }],
            capacity: BTreeMap::new(),
            attributes: BTreeMap::new(),
            state: semantic::ResourceState::Ready,
            reason_code: "READY".to_string(),
            summary: "ready".to_string(),
            links: Vec::new(),
        };

        let query = semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![semantic::CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::new(),
            }],
            minimum_capacity: BTreeMap::new(),
        };

        let principal = semantic::Principal {
            identity: semantic::Identity {
                id: "principal-golden-c".to_string(),
                generation: 1,
            },
        };

        let context = |req: &str| AuthorityCallContext {
            contract: semantic::ContractRevision::current(),
            namespace: NamespaceId::default(),
            request_id: req.to_string(),
            idempotency_key: req.to_string(),
        };

        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("golden_c_runtime.jsonl");
        let journal = Arc::new(FileRuntimeJournal::open(&journal_path).unwrap());

        let sandbox = ControlledRecoverySandbox {
            observed: Arc::new(Mutex::new(Vec::new())),
            reaped: Arc::new(Mutex::new(Vec::new())),
        };

        // ==========================================
        // 1. Epoch N: Launch Worker, Lease, Endpoint
        // ==========================================
        let epoch_n = journal.begin_epoch("node-golden-c").unwrap();
        let hardware_n = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let resources_n = Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-golden-c",
            vec![resource.clone()],
            epoch_n.next_fence_token,
        ));
        let adapter_n = KernelServiceAdapter::new(
            Arc::new(KernelDaemon::new(
                hardware_n.clone(),
                hardware_n,
                resources_n,
                Arc::new(sandbox.clone()),
                "node-golden-c",
                epoch_n.node_epoch,
            )),
            Arc::new(TestWorkerResolver),
        )
        .with_runtime_journal(journal.clone())
        .with_event_store(journal.clone());

        let authority_n = adapter_n.authority();
        let worker_id = semantic::Identity {
            id: "worker-golden-c".to_string(),
            generation: 1,
        };

        let lease_n = authority_n
            .acquire_lease(
                &context("lease-c"),
                &principal,
                worker_id.clone(),
                query.clone(),
                now_millis() + 60_000,
            )
            .unwrap();
        let fence_n = lease_n.fence_token;

        let worker_n = semantic::Worker {
            identity: worker_id.clone(),
            principal: principal.identity.clone(),
            provider: resource.provider.clone(),
            lease: lease_n.identity.clone(),
            state: semantic::WorkerState::Registered,
            execution_ref: "exec-c".to_string(),
            limits: BTreeMap::new(),
        };
        authority_n
            .start_worker(&context("worker-c"), &principal, worker_n.clone())
            .unwrap();

        let endpoint_n = semantic::Endpoint {
            identity: semantic::Identity {
                id: "endpoint-golden-c".to_string(),
                generation: 1,
            },
            provider: resource.provider.clone(),
            owner: worker_id.clone(),
            transport: "transport.uds".to_string(),
            schema_id: "schema.v1".to_string(),
            capabilities: Vec::new(),
            public_attributes: BTreeMap::new(),
        };
        authority_n
            .publish_endpoint(&context("ep-c"), &principal, endpoint_n.clone())
            .unwrap();

        let snapshot_n = authority_n.snapshot(&context("snap-c"), &principal).unwrap();
        assert_eq!(snapshot_n.workers.len(), 1);
        assert_eq!(snapshot_n.leases.len(), 1);
        assert_eq!(snapshot_n.endpoints.len(), 1);
        let cursor_n = snapshot_n.cursor.clone();

        // ==========================================
        // 2. Kill Kernel Unexpectedly (Crash Simulation)
        // ==========================================
        drop(authority_n);
        drop(adapter_n);

        // ==========================================
        // 3. Restart in Epoch N+1 and Recover
        // ==========================================
        let epoch_n_plus_one = journal.begin_epoch("node-golden-c").unwrap();
        assert!(epoch_n_plus_one.node_epoch > epoch_n.node_epoch, "Node epoch advanced");
        assert!(epoch_n_plus_one.next_fence_token > fence_n, "Next fence token strictly advanced");

        // Verify recovery classification:
        // Add a foreign process evidence to sandbox observations to verify foreign is NOT killed
        let foreign_evidence = RuntimeProcessEvidence {
            cgroup_name: "instance-foreign".to_string(),
            pid: 9999,
            start_time_ticks: 88888,
        };
        sandbox.observed.lock().unwrap().push(foreign_evidence.clone());

        let candidates = FileRuntimeJournal::classify_recovery(
            &epoch_n_plus_one,
            &sandbox.observed.lock().unwrap(),
        );
        let stale_cand = candidates.iter().find(|c| c.classification == RecoveryClassification::Stale);
        let foreign_cand = candidates.iter().find(|c| c.classification == RecoveryClassification::Foreign);
        assert!(stale_cand.is_some(), "Exact leftover evidence classified as Stale");
        assert!(foreign_cand.is_some(), "Foreign process classified as Foreign");

        // Stale process is reaped with exact evidence
        let exact_stale_evidence = stale_cand.unwrap().observed.clone().unwrap();
        sandbox.observed.lock().unwrap().retain(|e| e != &foreign_evidence); // remove foreign before startup gate
        journal
            .recover_before_listeners("node-golden-c", &epoch_n_plus_one, &sandbox)
            .unwrap();

        assert_eq!(
            *sandbox.reaped.lock().unwrap(),
            vec![exact_stale_evidence],
            "Only exact stale evidence was reaped"
        );

        // ==========================================
        // 4. Start Fresh Kernel in Epoch N+1
        // ==========================================
        let hardware_n_plus_one = Arc::new(TestHardware {
            resource: resource.clone(),
        });
        let resources_n_plus_one = Arc::new(InMemoryResourceManager::with_next_fence_token(
            "node-golden-c",
            vec![resource.clone()],
            epoch_n_plus_one.next_fence_token,
        ));
        let adapter_n_plus_one = KernelServiceAdapter::new(
            Arc::new(KernelDaemon::new(
                hardware_n_plus_one.clone(),
                hardware_n_plus_one,
                resources_n_plus_one,
                Arc::new(sandbox),
                "node-golden-c",
                epoch_n_plus_one.node_epoch,
            )),
            Arc::new(TestWorkerResolver),
        )
        .with_runtime_journal(journal.clone())
        .with_event_store(journal);

        let authority_n_plus_one = adapter_n_plus_one.authority();

        // Verify old authority is NOT silently adopted
        let fresh_snapshot = authority_n_plus_one.snapshot(&context("fresh-snap"), &principal).unwrap();
        assert!(fresh_snapshot.workers.is_empty(), "Old worker must NOT be silently adopted");
        assert!(fresh_snapshot.endpoints.is_empty(), "Old endpoint must NOT be silently adopted");
        assert!(fresh_snapshot.leases.is_empty(), "Old lease must NOT be silently adopted");

        // Verify old cursor gets SOURCE_CHANGED
        let source_changed = authority_n_plus_one
            .events_after(&context("replay-old-cursor"), &principal, &cursor_n, 256)
            .unwrap();
        assert_eq!(
            source_changed.status,
            semantic::ReplayStatus::SourceChanged,
            "Replay from old epoch cursor must return SourceChanged"
        );
        assert!(source_changed.events.is_empty());

        // Verify replacement Fence is strictly newer
        let replacement_lease = authority_n_plus_one
            .acquire_lease(
                &context("repl-lease"),
                &principal,
                semantic::Identity {
                    id: "worker-golden-c-new".to_string(),
                    generation: 1,
                },
                query,
                now_millis() + 60_000,
            )
            .unwrap();
        assert_eq!(replacement_lease.state, semantic::LeaseState::Active);
        assert!(replacement_lease.fence_token > fence_n, "Replacement fence must be strictly newer than pre-crash fence");
        assert!(replacement_lease.fence_token >= epoch_n_plus_one.next_fence_token);
    }

    /// Golden Test D — PID reuse / stale evidence
    ///
    /// Scenario:
    /// Exercise or simulate matching PID with mismatched process-start identity
    /// (e.g. matching PID but mismatched start_time_ticks or cgroup).
    ///
    /// Verifies:
    /// - Recovery classifies reality correctly (Foreign for live process, Unknown for old record);
    /// - Recovery must not claim it as the old Worker;
    /// - Foreign process is never killed / never reaped;
    /// - Recovery fails closed rather than adopting or destroying foreign state.
    #[test]
    fn golden_test_d_pid_reuse_stale_evidence_protects_foreign_process() {
        use cy_kernel_api::{
            CleanupReport, DeviceBinding, LaunchPlan, NodeCapabilities, ProcessHandle,
            ProcessRuntime, StopRequest,
        };

        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("golden_d_runtime.jsonl");
        let journal = FileRuntimeJournal::open(&journal_path).unwrap();

        // 1. Durably record a worker launch in epoch 1 with PID 4040, ticks 10_000
        let epoch_1 = journal.begin_epoch("node-golden-d").unwrap();
        journal
            .append(RuntimeJournalRecord {
                event: RuntimeJournalEvent::InstanceLaunched,
                node_id: "node-golden-d".to_string(),
                node_epoch: epoch_1.node_epoch,
                instance_name: Some("worker-pid-reuse".to_string()),
                lease_name: Some("lease-pid-reuse".to_string()),
                fence_token: Some(42),
                reason_code: "LAUNCHED".to_string(),
                runtime_evidence: Some(RuntimeProcessEvidence {
                    cgroup_name: "instance-worker-pid-reuse".to_string(),
                    pid: 4040,
                    start_time_ticks: 10_000,
                }),
            })
            .unwrap();

        // 2. Kernel restarts in epoch 2
        let epoch_2 = journal.begin_epoch("node-golden-d").unwrap();
        assert_eq!(epoch_2.runtime_processes.len(), 1);
        assert_eq!(epoch_2.runtime_processes[0].instance_name, "worker-pid-reuse");

        // 3. Observed in sandbox: an OS process with matching PID 4040, BUT ticks = 99_999 (PID reused!)
        let reused_pid_evidence = RuntimeProcessEvidence {
            cgroup_name: "instance-worker-pid-reuse".to_string(),
            pid: 4040,
            start_time_ticks: 99_999, // Mismatched process start time!
        };

        // 4. Verify classification:
        // - Live reused process is classified as Foreign (NOT Stale!)
        // - Stale journal record is classified as Unknown
        let candidates = FileRuntimeJournal::classify_recovery(&epoch_2, &[reused_pid_evidence.clone()]);
        assert_eq!(candidates.len(), 2);
        let foreign_cand = candidates.iter().find(|c| c.classification == RecoveryClassification::Foreign);
        let unknown_cand = candidates.iter().find(|c| c.classification == RecoveryClassification::Unknown);
        assert!(foreign_cand.is_some(), "Reused PID with mismatched start ticks must be classified as Foreign");
        assert!(unknown_cand.is_some(), "Old record without exact live match must be classified as Unknown");

        // 5. Test recovery execution:
        #[derive(Default)]
        struct MockPidReuseSandbox {
            reaped: Mutex<Vec<RuntimeProcessEvidence>>,
            observed: Vec<RuntimeProcessEvidence>,
        }

        impl ProcessRuntime for MockPidReuseSandbox {
            fn preflight(&self) -> NodeCapabilities {
                NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                }
            }
            fn launch(&self, _plan: &LaunchPlan, _binding: &DeviceBinding) -> Result<ProcessHandle, ProviderError> {
                Err(ProviderError::new("mock", "UNUSED", "unused"))
            }
            fn stop(&self, _handle: &ProcessHandle, _request: &StopRequest) -> Result<CleanupReport, ProviderError> {
                Err(ProviderError::new("mock", "UNUSED", "unused"))
            }
        }

        impl SandboxBackend for MockPidReuseSandbox {
            fn backend_id(&self) -> &str {
                "mock-pid-reuse"
            }
            fn discover_recovery_processes(&self) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
                Ok(self.observed.clone())
            }
            fn recover_stale_process(&self, evidence: &RuntimeProcessEvidence) -> Result<CleanupReport, ProviderError> {
                self.reaped.lock().unwrap().push(evidence.clone());
                Ok(CleanupReport {
                    complete: true,
                    exit_code: Some(0),
                    oom_killed: false,
                    conditions: Vec::new(),
                    reason_code: "REAPED".to_string(),
                })
            }
        }

        let sandbox = MockPidReuseSandbox {
            reaped: Mutex::new(Vec::new()),
            observed: vec![reused_pid_evidence.clone()],
        };

        // Recovery MUST fail closed and MUST NOT reap the foreign reused PID process!
        let recovery_result = journal.recover_before_listeners("node-golden-d", &epoch_2, &sandbox);
        assert!(recovery_result.is_err(), "Recovery must fail closed on foreign/unmatched process");
        let error = recovery_result.unwrap_err();
        assert!(
            matches!(error.reason_code.as_str(), "RECOVERY_FOREIGN_PROCESS" | "RECOVERY_UNKNOWN_PROCESS"),
            "Error code must indicate unverified runtime state: {}", error.reason_code
        );

        // Verification: The foreign process was NEVER reaped!
        assert!(
            sandbox.reaped.lock().unwrap().is_empty(),
            "Recovery must NOT reap or terminate the foreign reused-PID process"
        );
    }
}

