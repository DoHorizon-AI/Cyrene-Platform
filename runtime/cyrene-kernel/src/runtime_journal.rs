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

use cy_kernel_api::{ProviderError, RuntimeJournalEvent, RuntimeJournalRecord, RuntimeJournalSink};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryState {
    /// The newly persisted epoch for this Kernel process.
    pub node_epoch: u64,
    pub next_fence_token: u64,
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
        })?;
        Ok(RecoveryState {
            node_epoch,
            next_fence_token: recovery.next_fence_token,
        })
    }

    pub fn recover(&self, node_id: &str) -> std::io::Result<RecoveryState> {
        if !self.path.exists() {
            return Ok(RecoveryState {
                node_epoch: 0,
                next_fence_token: 1,
            });
        }
        let mut node_epoch = 0_u64;
        let mut max_fence_token = 0_u64;
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
            }
        }
        Ok(RecoveryState {
            node_epoch,
            next_fence_token: max_fence_token.saturating_add(1).max(1),
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

fn event_name(event: RuntimeJournalEvent) -> &'static str {
    match event {
        RuntimeJournalEvent::KernelStarted => "KERNEL_STARTED",
        RuntimeJournalEvent::LeaseReserved => "LEASE_RESERVED",
        RuntimeJournalEvent::LeaseReleased => "LEASE_RELEASED",
        RuntimeJournalEvent::InstanceLaunched => "INSTANCE_LAUNCHED",
        RuntimeJournalEvent::InstanceTerminated => "INSTANCE_TERMINATED",
        RuntimeJournalEvent::InstanceCleanupFailed => "INSTANCE_CLEANUP_FAILED",
        RuntimeJournalEvent::WatchdogReaped => "WATCHDOG_REAPED",
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
            })
            .unwrap();
        let next = journal.begin_epoch("node-1").unwrap();
        assert!(next.node_epoch > first.node_epoch);
        assert_eq!(next.next_fence_token, 42);
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
}
