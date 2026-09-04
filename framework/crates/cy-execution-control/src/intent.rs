//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 intent.rs                                                       │
//! │  Module: cy_execution_control::intent                               │
//! │  Role: Durable execution-intent deduplication and recovery evidence. │
//! │                                                                     │
//! │  模块职责：持久化执行意图去重状态与恢复证据，不复制 Lease authority。    │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use cy_kernel_contract as semantic;
use cy_proto::core_v1::NodeRef;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const LEDGER_SCHEMA_VERSION: u32 = 1;

/// Durable state of one caller-owned execution intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentDisposition {
    Pending,
    LeaseAcquired,
    AssignmentDispatching,
    Completed,
    Released,
    Failed,
    UnknownRequiresReconciliation,
}

/// Non-authoritative projection used to correlate recovery with Kernel facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct IdentityEvidence {
    id: String,
    generation: u64,
}

impl From<&semantic::Identity> for IdentityEvidence {
    fn from(identity: &semantic::Identity) -> Self {
        Self {
            id: identity.id.clone(),
            generation: identity.generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AuthorityEvidence {
    node_id: String,
    node_epoch: u64,
    lease_id: String,
    lease_generation: u64,
    holder_id: String,
    holder_generation: u64,
    resources: Vec<IdentityEvidence>,
    fence_token: u64,
    expires_at_unix_ms: Option<u64>,
}

impl AuthorityEvidence {
    fn from_authority(node: &NodeRef, lease: &semantic::Lease) -> Self {
        Self {
            node_id: node.node_id.clone(),
            node_epoch: node.node_epoch,
            lease_id: lease.identity.id.clone(),
            lease_generation: lease.identity.generation,
            holder_id: lease.holder.id.clone(),
            holder_generation: lease.holder.generation,
            resources: lease.resources.iter().map(IdentityEvidence::from).collect(),
            fence_token: lease.fence_token,
            expires_at_unix_ms: lease.expires_at_unix_ms,
        }
    }

    fn matches(&self, node: &NodeRef, lease: &semantic::Lease) -> bool {
        self.node_id == node.node_id
            && self.node_epoch == node.node_epoch
            && self.lease_id == lease.identity.id
            && self.lease_generation == lease.identity.generation
            && self.holder_id == lease.holder.id
            && self.holder_generation == lease.holder.generation
            && self.resources
                == lease
                    .resources
                    .iter()
                    .map(IdentityEvidence::from)
                    .collect::<Vec<_>>()
            && self.fence_token == lease.fence_token
            && self.expires_at_unix_ms == lease.expires_at_unix_ms
    }
}

/// Inspectable durable record. It stores correlation evidence, never Resource
/// ownership or a second writable Lease state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIntentRecord {
    assignment_id: String,
    payload_digest: String,
    disposition: IntentDisposition,
    authority_evidence: Option<AuthorityEvidence>,
    revision: u64,
    updated_at_unix_ms: u64,
}

impl ExecutionIntentRecord {
    pub fn assignment_id(&self) -> &str {
        &self.assignment_id
    }

    pub fn payload_digest(&self) -> &str {
        &self.payload_digest
    }

    pub fn disposition(&self) -> IntentDisposition {
        self.disposition
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn updated_at_unix_ms(&self) -> u64 {
        self.updated_at_unix_ms
    }

    pub fn has_lease_observation(&self) -> bool {
        self.authority_evidence.is_some()
    }

    pub(crate) fn pending(
        assignment_id: &str,
        payload_digest: &str,
        now_unix_ms: u64,
    ) -> Result<Self, IntentStoreError> {
        let record = Self {
            assignment_id: assignment_id.to_string(),
            payload_digest: payload_digest.to_string(),
            disposition: IntentDisposition::Pending,
            authority_evidence: None,
            revision: 1,
            updated_at_unix_ms: now_unix_ms,
        };
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn transition(
        &self,
        disposition: IntentDisposition,
        authority: Option<(&NodeRef, &semantic::Lease)>,
        now_unix_ms: u64,
    ) -> Result<Self, IntentStoreError> {
        if !legal_transition(self.disposition, disposition) {
            return Err(IntentStoreError::conflict(format!(
                "illegal intent transition {:?} -> {disposition:?}",
                self.disposition
            )));
        }
        let supplied =
            authority.map(|(node, lease)| AuthorityEvidence::from_authority(node, lease));
        if let (Some(existing), Some(candidate)) = (&self.authority_evidence, &supplied) {
            if existing != candidate {
                return Err(IntentStoreError::conflict(
                    "intent transition attempted to replace immutable Lease evidence",
                ));
            }
        }
        let authority_evidence = self.authority_evidence.clone().or(supplied);
        if matches!(
            disposition,
            IntentDisposition::LeaseAcquired
                | IntentDisposition::AssignmentDispatching
                | IntentDisposition::Completed
                | IntentDisposition::Released
        ) && authority_evidence.is_none()
        {
            return Err(IntentStoreError::invalid(
                "intent state requires an authority-returned Lease observation",
            ));
        }
        let next = Self {
            assignment_id: self.assignment_id.clone(),
            payload_digest: self.payload_digest.clone(),
            disposition,
            authority_evidence,
            revision: self
                .revision
                .checked_add(1)
                .ok_or_else(|| IntentStoreError::invalid("intent record revision overflowed"))?,
            updated_at_unix_ms: now_unix_ms,
        };
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn matches_authority(&self, node: &NodeRef, lease: &semantic::Lease) -> bool {
        self.authority_evidence
            .as_ref()
            .is_some_and(|observation| observation.matches(node, lease))
    }

    fn validate(&self) -> Result<(), IntentStoreError> {
        if self.assignment_id.is_empty() || self.revision == 0 || self.updated_at_unix_ms == 0 {
            return Err(IntentStoreError::invalid(
                "intent identity, revision, and update time are required",
            ));
        }
        validate_digest(&self.payload_digest)?;
        if let Some(observation) = &self.authority_evidence {
            if observation.node_id.is_empty()
                || observation.node_epoch == 0
                || observation.lease_id.is_empty()
                || observation.lease_generation == 0
                || observation.holder_id.is_empty()
                || observation.holder_generation == 0
                || observation.resources.is_empty()
                || observation
                    .resources
                    .iter()
                    .any(|resource| resource.id.is_empty() || resource.generation == 0)
                || observation.resources.iter().collect::<BTreeSet<_>>().len()
                    != observation.resources.len()
                || observation.fence_token == 0
            {
                return Err(IntentStoreError::invalid(
                    "persisted Lease observation contains an invalid identity or fence",
                ));
            }
        }
        if matches!(self.disposition, IntentDisposition::Pending)
            && self.authority_evidence.is_some()
        {
            return Err(IntentStoreError::invalid(
                "pending intent cannot contain Lease evidence",
            ));
        }
        if matches!(
            self.disposition,
            IntentDisposition::LeaseAcquired
                | IntentDisposition::AssignmentDispatching
                | IntentDisposition::Completed
                | IntentDisposition::Released
        ) && self.authority_evidence.is_none()
        {
            return Err(IntentStoreError::invalid(
                "persisted intent state requires Lease evidence",
            ));
        }
        Ok(())
    }
}

fn legal_transition(current: IntentDisposition, next: IntentDisposition) -> bool {
    use IntentDisposition::{
        AssignmentDispatching, Completed, Failed, LeaseAcquired, Pending, Released,
        UnknownRequiresReconciliation,
    };
    matches!(
        (current, next),
        (
            Pending,
            LeaseAcquired | Failed | UnknownRequiresReconciliation
        ) | (
            LeaseAcquired,
            AssignmentDispatching | Failed | UnknownRequiresReconciliation
        ) | (
            AssignmentDispatching,
            Completed | Failed | UnknownRequiresReconciliation
        ) | (Completed, Released | UnknownRequiresReconciliation)
    )
}

fn validate_digest(value: &str) -> Result<(), IntentStoreError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(IntentStoreError::invalid(
            "intent payload digest must be lowercase sha256:<hex>",
        ))
    }
}

/// Stable class of intent-store failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentStoreErrorKind {
    AlreadyExists,
    NotFound,
    RevisionConflict,
    InvalidRecord,
    Persistence,
}

/// Fail-closed storage error surfaced at the control boundary.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {message}")]
pub struct IntentStoreError {
    pub kind: IntentStoreErrorKind,
    pub message: String,
}

impl IntentStoreError {
    fn already_exists(message: impl Into<String>) -> Self {
        Self {
            kind: IntentStoreErrorKind::AlreadyExists,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: IntentStoreErrorKind::NotFound,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: IntentStoreErrorKind::RevisionConflict,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: IntentStoreErrorKind::InvalidRecord,
            message: message.into(),
        }
    }

    fn persistence(message: impl Into<String>) -> Self {
        Self {
            kind: IntentStoreErrorKind::Persistence,
            message: message.into(),
        }
    }
}

/// Caller-owned durable compare-and-set store. Implementations must retain
/// terminal records as tombstones and must not reinterpret Lease evidence.
pub trait ExecutionIntentStore: Send + Sync {
    fn load(&self, assignment_id: &str) -> Result<Option<ExecutionIntentRecord>, IntentStoreError>;

    fn create(&self, record: ExecutionIntentRecord) -> Result<(), IntentStoreError>;

    fn compare_and_set(
        &self,
        expected_revision: u64,
        record: ExecutionIntentRecord,
    ) -> Result<(), IntentStoreError>;
}

/// Explicit test/reference store. Production composition should use a durable
/// implementation such as [`FileExecutionIntentStore`].
#[derive(Debug, Default)]
pub struct InMemoryExecutionIntentStore {
    records: Mutex<BTreeMap<String, ExecutionIntentRecord>>,
}

impl ExecutionIntentStore for InMemoryExecutionIntentStore {
    fn load(&self, assignment_id: &str) -> Result<Option<ExecutionIntentRecord>, IntentStoreError> {
        Ok(self
            .records
            .lock()
            .expect("in-memory intent store lock poisoned")
            .get(assignment_id)
            .cloned())
    }

    fn create(&self, record: ExecutionIntentRecord) -> Result<(), IntentStoreError> {
        record.validate()?;
        let mut records = self
            .records
            .lock()
            .expect("in-memory intent store lock poisoned");
        if records.contains_key(record.assignment_id()) {
            return Err(IntentStoreError::already_exists(
                "execution intent already has a durable tombstone",
            ));
        }
        records.insert(record.assignment_id.clone(), record);
        Ok(())
    }

    fn compare_and_set(
        &self,
        expected_revision: u64,
        record: ExecutionIntentRecord,
    ) -> Result<(), IntentStoreError> {
        record.validate()?;
        let mut records = self
            .records
            .lock()
            .expect("in-memory intent store lock poisoned");
        replace_record(&mut records, expected_revision, record)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedLedger {
    schema_version: u32,
    records: BTreeMap<String, ExecutionIntentRecord>,
}

struct PersistFailure {
    committed: bool,
    error: IntentStoreError,
}

/// Single-writer, crash-resistant JSON ledger using same-directory atomic
/// replacement and fsync. Multi-replica deployments should inject a database
/// implementation with equivalent compare-and-set semantics.
#[derive(Debug)]
pub struct FileExecutionIntentStore {
    path: PathBuf,
    records: Mutex<BTreeMap<String, ExecutionIntentRecord>>,
}

impl FileExecutionIntentStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, IntentStoreError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(IntentStoreError::invalid("intent ledger path is required"));
        }
        let records = if path.exists() {
            let bytes = fs::read(&path).map_err(|error| {
                IntentStoreError::persistence(format!("cannot read intent ledger: {error}"))
            })?;
            let ledger: PersistedLedger = serde_json::from_slice(&bytes).map_err(|error| {
                IntentStoreError::persistence(format!("cannot decode intent ledger: {error}"))
            })?;
            if ledger.schema_version != LEDGER_SCHEMA_VERSION {
                return Err(IntentStoreError::persistence(format!(
                    "unsupported intent ledger schema {}",
                    ledger.schema_version
                )));
            }
            for (key, record) in &ledger.records {
                record.validate()?;
                if key != record.assignment_id() {
                    return Err(IntentStoreError::persistence(
                        "intent ledger key does not match its record identity",
                    ));
                }
            }
            ledger.records
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            records: Mutex::new(records),
        })
    }

    fn persist(
        &self,
        records: &BTreeMap<String, ExecutionIntentRecord>,
    ) -> Result<(), PersistFailure> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(PersistFailure {
                committed: false,
                error: IntentStoreError::persistence(
                    "intent ledger parent directory does not exist",
                ),
            });
        }
        let ledger = PersistedLedger {
            schema_version: LEDGER_SCHEMA_VERSION,
            records: records.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&ledger).map_err(|error| PersistFailure {
            committed: false,
            error: IntentStoreError::persistence(format!("cannot encode intent ledger: {error}")),
        })?;
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PersistFailure {
                committed: false,
                error: IntentStoreError::invalid("intent ledger file name is invalid"),
            })?;
        let temporary = parent.join(format!(".{file_name}-{}.tmp", Uuid::new_v4()));
        let mut committed = false;
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            committed = true;
            File::open(parent)?.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(error) = result {
            if !committed {
                let _ = fs::remove_file(&temporary);
            }
            return Err(PersistFailure {
                committed,
                error: IntentStoreError::persistence(format!(
                    "cannot durably replace intent ledger: {error}"
                )),
            });
        }
        Ok(())
    }
}

impl ExecutionIntentStore for FileExecutionIntentStore {
    fn load(&self, assignment_id: &str) -> Result<Option<ExecutionIntentRecord>, IntentStoreError> {
        Ok(self
            .records
            .lock()
            .expect("file intent store lock poisoned")
            .get(assignment_id)
            .cloned())
    }

    fn create(&self, record: ExecutionIntentRecord) -> Result<(), IntentStoreError> {
        record.validate()?;
        let mut records = self
            .records
            .lock()
            .expect("file intent store lock poisoned");
        if records.contains_key(record.assignment_id()) {
            return Err(IntentStoreError::already_exists(
                "execution intent already has a durable tombstone",
            ));
        }
        records.insert(record.assignment_id.clone(), record.clone());
        if let Err(failure) = self.persist(&records) {
            if !failure.committed {
                records.remove(record.assignment_id());
            }
            return Err(failure.error);
        }
        Ok(())
    }

    fn compare_and_set(
        &self,
        expected_revision: u64,
        record: ExecutionIntentRecord,
    ) -> Result<(), IntentStoreError> {
        record.validate()?;
        let mut records = self
            .records
            .lock()
            .expect("file intent store lock poisoned");
        let previous = records.get(record.assignment_id()).cloned();
        replace_record(&mut records, expected_revision, record.clone())?;
        if let Err(failure) = self.persist(&records) {
            if !failure.committed {
                if let Some(previous) = previous {
                    records.insert(previous.assignment_id.clone(), previous);
                }
            }
            return Err(failure.error);
        }
        Ok(())
    }
}

fn replace_record(
    records: &mut BTreeMap<String, ExecutionIntentRecord>,
    expected_revision: u64,
    record: ExecutionIntentRecord,
) -> Result<(), IntentStoreError> {
    let existing = records.get(record.assignment_id()).ok_or_else(|| {
        IntentStoreError::not_found("execution intent has no durable pending record")
    })?;
    if existing.revision != expected_revision
        || record.revision != expected_revision.saturating_add(1)
    {
        return Err(IntentStoreError::conflict(
            "execution intent revision changed concurrently",
        ));
    }
    if existing.payload_digest != record.payload_digest {
        return Err(IntentStoreError::conflict(
            "execution intent payload digest is immutable",
        ));
    }
    records.insert(record.assignment_id.clone(), record);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    fn lease() -> semantic::Lease {
        semantic::Lease {
            identity: semantic::Identity {
                id: "lease-1".to_string(),
                generation: 1,
            },
            holder: semantic::Identity {
                id: "runtime-1".to_string(),
                generation: 1,
            },
            resources: vec![semantic::Identity {
                id: "resource-1".to_string(),
                generation: 1,
            }],
            state: semantic::LeaseState::Active,
            fence_token: 9,
            expires_at_unix_ms: Some(20_000),
        }
    }

    fn node() -> NodeRef {
        NodeRef {
            node_id: "node-1".to_string(),
            node_epoch: 1,
        }
    }

    #[test]
    fn terminal_record_is_a_durable_tombstone_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("execution-intents.json");
        let store = FileExecutionIntentStore::open(&path).unwrap();
        let pending = ExecutionIntentRecord::pending("assignment-1", &digest(), 1_000).unwrap();
        store.create(pending.clone()).unwrap();
        let acquired = pending
            .transition(
                IntentDisposition::LeaseAcquired,
                Some((&node(), &lease())),
                1_001,
            )
            .unwrap();
        store
            .compare_and_set(pending.revision(), acquired.clone())
            .unwrap();
        let dispatching = acquired
            .transition(IntentDisposition::AssignmentDispatching, None, 1_002)
            .unwrap();
        store
            .compare_and_set(acquired.revision(), dispatching.clone())
            .unwrap();
        let completed = dispatching
            .transition(IntentDisposition::Completed, None, 1_003)
            .unwrap();
        store
            .compare_and_set(dispatching.revision(), completed)
            .unwrap();
        drop(store);

        let reopened = FileExecutionIntentStore::open(path).unwrap();
        let record = reopened.load("assignment-1").unwrap().unwrap();
        assert_eq!(record.disposition(), IntentDisposition::Completed);
        assert!(record.matches_authority(&node(), &lease()));
        assert_eq!(
            reopened
                .create(ExecutionIntentRecord::pending("assignment-1", &digest(), 2_000).unwrap())
                .unwrap_err()
                .kind,
            IntentStoreErrorKind::AlreadyExists
        );
    }

    #[test]
    fn lease_evidence_cannot_change_across_transitions() {
        let pending = ExecutionIntentRecord::pending("assignment-1", &digest(), 1_000).unwrap();
        let acquired = pending
            .transition(
                IntentDisposition::LeaseAcquired,
                Some((&node(), &lease())),
                1_001,
            )
            .unwrap();
        let mut other = lease();
        other.fence_token += 1;
        assert_eq!(
            acquired
                .transition(
                    IntentDisposition::AssignmentDispatching,
                    Some((&node(), &other)),
                    1_002,
                )
                .unwrap_err()
                .kind,
            IntentStoreErrorKind::RevisionConflict
        );

        let mut different_resource = lease();
        different_resource.resources[0].id = "resource-other".to_string();
        assert!(!acquired.matches_authority(&node(), &different_resource));
        assert_eq!(
            acquired
                .transition(
                    IntentDisposition::AssignmentDispatching,
                    Some((&node(), &different_resource)),
                    1_002,
                )
                .unwrap_err()
                .kind,
            IntentStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn persisted_state_requires_consistent_lease_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("execution-intents.json");
        let invalid = format!(
            r#"{{
  "schema_version": 1,
  "records": {{
    "assignment-1": {{
      "assignment_id": "assignment-1",
      "payload_digest": "{}",
      "disposition": "completed",
      "authority_evidence": null,
      "revision": 4,
      "updated_at_unix_ms": 1000
    }}
  }}
}}"#,
            digest()
        );
        fs::write(&path, invalid).unwrap();

        assert_eq!(
            FileExecutionIntentStore::open(path).unwrap_err().kind,
            IntentStoreErrorKind::InvalidRecord
        );
    }
}
