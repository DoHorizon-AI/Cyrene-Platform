// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-runtime-maintenance/src/lib.rs              ║
// ║ Module: cy_runtime_maintenance                                       ║
// ║ Role: Durable, cross-process task admission and runtime update gate.   ║
// ║                                                                      ║
// ║ 模块职责：跨进程持久化任务准入与运行时更新门禁。                       ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Durable admission and maintenance state shared by Kernel, broker, and SDK clients.
//!
//! A single advisory file lock serializes every task admission, task transition,
//! readiness check, and maintenance transition across processes. The append-only
//! journal is authoritative; a state snapshot accelerates reads and is repaired
//! from journal records after a crash between the journal and snapshot writes.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const STATE_FILE: &str = "maintenance-state.json";
const JOURNAL_FILE: &str = "maintenance-journal.jsonl";
const LOCK_FILE: &str = "maintenance.lock";
const STATE_MIGRATION_MARKER_FILE: &str = "maintenance-schema-migration.json";
const OPERATOR_TOKEN_HASH_FILE: &str = "operator-token.sha256";
const STATE_SCHEMA_VERSION: u32 = 2;
const LEGACY_STATE_SCHEMA_VERSION: u32 = 1;
const ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION: u32 = 1;
const DEFAULT_SOURCE_STALENESS: Duration = Duration::from_secs(30);
const JOURNAL_COMPACTION_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Shared persistence cohort version advertised by Kernel and the broker.
pub const STATE_PROTOCOL_VERSION: &str = "cyrene.runtime-maintenance.state.v2";

/// Classifies which installed component set an update will modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpdateTargetKind {
    /// Product/package files can be replaced without restarting Kernel or sandboxd.
    PackageOnly,
    /// Kernel or sandboxd will be restarted and all runtime allocations must be released.
    CoreRuntime,
}

/// Readiness decision shared by CLI, UI, Product, and Kernel callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadinessStatus {
    /// All trusted sources were fresh and the requested transition is safe.
    Ready,
    /// At least one accepted task remains in a nonterminal state.
    ActiveTasks,
    /// At least one authorized package binding operation is still in flight.
    ActiveBindingOperations,
    /// A trusted source, catalog, state file, or runtime fact could not be verified.
    Unknown,
    /// Kernel/sandboxd would restart while a Worker, Lease, or allocation remains.
    IdleRuntimeRequiresUnload,
    /// Another maintenance transaction has already closed admission.
    MaintenanceActive,
    /// The preview generation changed before the apply transaction began.
    StaleReadiness,
    /// Restart requires explicit user confirmation.
    UserConfirmationRequired,
}

/// Nonterminal task lifecycle states recognized by the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskActivityState {
    Accepted,
    Queued,
    Dispatching,
    Running,
    Canceling,
    Inflight,
}

impl TaskActivityState {
    /// Parses the stable JSON/RPC state name.
    pub fn parse(value: &str) -> Result<Self, MaintenanceError> {
        match value {
            "ACCEPTED" => Ok(Self::Accepted),
            "QUEUED" => Ok(Self::Queued),
            "DISPATCHING" => Ok(Self::Dispatching),
            "RUNNING" => Ok(Self::Running),
            "CANCELING" => Ok(Self::Canceling),
            "INFLIGHT" => Ok(Self::Inflight),
            _ => Err(MaintenanceError::InvalidRequest(format!(
                "unsupported nonterminal task state: {value}"
            ))),
        }
    }
}

/// Durable source record written by the root-managed installation catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedActivitySource {
    /// Stable lowercase catalog ID used in Product task records.
    pub source_id: String,
    /// Expected Unix UID for local broker calls made by this source.
    pub uid: u32,
    /// Optional expected Unix GID for local broker calls made by this source.
    #[serde(default)]
    pub gid: Option<u32>,
    /// SHA-256 of the read-only token mounted into this source only.
    pub source_token_sha256: String,
    /// Exact package binding operations this source may submit to the runtime.
    /// Missing scopes are treated as an empty allowlist.
    #[serde(default)]
    pub binding_scopes: Vec<TrustedBindingScope>,
}

/// One exact binding/package/install scope granted by the root-owned catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedBindingScope {
    /// Globally unique binding identity owned by this source.
    pub binding_id: String,
    /// Exact installed package authorized for this binding.
    pub package_id: String,
    /// Exact installation identities; an empty list grants no operation.
    #[serde(default)]
    pub installation_ids: Vec<String>,
    /// Operations allowed for this exact binding/package/installation set.
    #[serde(default)]
    pub operations: Vec<BindingOperationKind>,
}

/// Package runtime mutation admitted through the shared maintenance gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingOperationKind {
    Activate,
    Recover,
    Deactivate,
}

impl BindingOperationKind {
    /// Parses the narrow package-runtime operation vocabulary accepted by the gate.
    pub fn parse(value: &str) -> Result<Self, MaintenanceError> {
        match value {
            "activate" => Ok(Self::Activate),
            "recover" => Ok(Self::Recover),
            "deactivate" => Ok(Self::Deactivate),
            _ => Err(MaintenanceError::InvalidRequest(format!(
                "unsupported binding operation: {value}"
            ))),
        }
    }
}

/// Authenticated peer identity supplied by the broker for one binding operation.
/// The source token is checked against the currently loaded catalog under the gate lock.
#[derive(Clone)]
pub struct BindingOperationCaller {
    /// Source identity presented by the Product caller.
    pub source_id: String,
    /// Raw per-source token; it is checked but never persisted in gate state.
    pub source_token: String,
    /// Unix peer credentials sampled by the broker with `SO_PEERCRED`.
    pub peer_uid: u32,
    /// Unix peer group sampled by the broker with `SO_PEERCRED`.
    pub peer_gid: u32,
    /// Catalog generation observed by the caller's authority handshake.
    pub expected_catalog_generation: u64,
}

/// Exact binding scope attached to an admitted package runtime operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingOperationScope {
    /// Product binding whose runtime state is changing.
    pub binding_id: String,
    /// Package recorded by the daemon for the binding.
    pub package_id: String,
    /// Exact package installation involved in this operation.
    pub installation_id: String,
    /// Narrow mutation kind admitted by the root catalog.
    pub operation: BindingOperationKind,
}

/// Durable operation admission returned to the package runtime daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingOperationAdmission {
    /// Caller-provided idempotency key shared with the Package Runtime request.
    pub request_id: String,
    /// Authenticated Product activity source that owns the binding.
    pub source_id: String,
    /// Exact binding/package/installation/operation admitted by the catalog.
    pub scope: BindingOperationScope,
    /// Opaque completion capability returned only to the authenticated caller.
    pub operation_token: String,
    /// Catalog generation the broker validated for this reservation.
    pub catalog_generation: u64,
    /// Gate generation after the durable reservation was recorded.
    pub gate_generation: u64,
    /// An identical request still has a pending reservation and must not run again.
    pub already_in_flight: bool,
    /// A completed replay must not execute the package mutation again.
    pub already_completed: bool,
}

/// Non-secret active-operation detail included in update-readiness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingOperationActivity {
    /// Request id for the durable in-flight operation.
    pub request_id: String,
    /// Authenticated source that owns the operation.
    pub source_id: String,
    /// Non-secret scope reported to update-readiness callers.
    pub scope: BindingOperationScope,
    /// Unix timestamp recorded at reservation time; it is informational only.
    pub started_at_unix_ms: u64,
}

/// Result returned after the exact admitted operation scope is completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingOperationCompletion {
    /// Whether this call has durably completed the reservation.
    pub completed: bool,
    /// Gate generation after completion or idempotent replay.
    pub gate_generation: u64,
}

/// Versioned complete catalog of installed task-admission sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedActivitySourceCatalog {
    pub schema_version: u32,
    pub generation: u64,
    pub sources: Vec<TrustedActivitySource>,
}

impl TrustedActivitySourceCatalog {
    /// Loads and validates the root-owned installed-source catalog.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MaintenanceError> {
        let path = path.as_ref();
        reject_symlink_file(path)?;
        validate_catalog_permissions(path)?;
        let catalog: Self = serde_json::from_slice(&fs::read(path).map_err(|error| {
            MaintenanceError::CatalogUnavailable(format!("{}: {error}", path.display()))
        })?)
        .map_err(|error| {
            MaintenanceError::CatalogUnavailable(format!("{}: {error}", path.display()))
        })?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Builds the placeholder for a broker whose catalog file has never been
    /// provisioned. Generation zero fails every generation comparison and the
    /// empty source set trusts nothing, so the broker stays fail-closed until
    /// `init-catalog` installs a real catalog.
    fn unprovisioned() -> Self {
        Self {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: 0,
            sources: Vec::new(),
        }
    }

    /// Validates schema, source identifiers, uniqueness, and generation.
    pub fn validate(&self) -> Result<(), MaintenanceError> {
        if self.schema_version != ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION || self.generation == 0 {
            return Err(MaintenanceError::CatalogUnavailable(
                "unsupported source catalog schema or zero generation".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut binding_owners = BTreeMap::<&str, &str>::new();
        for source in &self.sources {
            validate_identifier(&source.source_id, "source_id")?;
            if source.source_token_sha256.len() != 64
                || !source
                    .source_token_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(MaintenanceError::CatalogUnavailable(format!(
                    "source {} has an invalid token hash",
                    source.source_id
                )));
            }
            if !ids.insert(source.source_id.as_str()) {
                return Err(MaintenanceError::CatalogUnavailable(format!(
                    "duplicate source_id: {}",
                    source.source_id
                )));
            }
            let mut scope_keys = BTreeSet::new();
            for scope in &source.binding_scopes {
                if binding_owners
                    .insert(&scope.binding_id, &source.source_id)
                    .is_some_and(|owner| owner != source.source_id)
                {
                    return Err(MaintenanceError::CatalogUnavailable(format!(
                        "binding {} is authorized by more than one source",
                        scope.binding_id
                    )));
                }
                validate_identifier(&scope.binding_id, "binding_id")?;
                validate_identifier(&scope.package_id, "package_id")?;
                if scope.installation_ids.is_empty() || scope.operations.is_empty() {
                    return Err(MaintenanceError::CatalogUnavailable(format!(
                        "source {} has an empty binding operation scope",
                        source.source_id
                    )));
                }
                if !scope_keys.insert((scope.binding_id.as_str(), scope.package_id.as_str())) {
                    return Err(MaintenanceError::CatalogUnavailable(format!(
                        "source {} has a duplicate binding/package scope",
                        source.source_id
                    )));
                }
                let mut installation_ids = BTreeSet::new();
                for installation_id in &scope.installation_ids {
                    validate_identifier(installation_id, "installation_id")?;
                    if !installation_ids.insert(installation_id.as_str()) {
                        return Err(MaintenanceError::CatalogUnavailable(format!(
                            "source {} has a duplicate installation id in binding scope {}",
                            source.source_id, scope.binding_id
                        )));
                    }
                }
                let mut operations = BTreeSet::new();
                if scope
                    .operations
                    .iter()
                    .any(|operation| !operations.insert(*operation))
                {
                    return Err(MaintenanceError::CatalogUnavailable(format!(
                        "source {} has a duplicate operation in binding scope {}",
                        source.source_id, scope.binding_id
                    )));
                }
            }
        }
        Ok(())
    }

    fn source_ids(&self) -> BTreeSet<String> {
        self.sources
            .iter()
            .map(|source| source.source_id.clone())
            .collect()
    }

    fn source(&self, source_id: &str) -> Option<&TrustedActivitySource> {
        self.sources
            .iter()
            .find(|source| source.source_id == source_id)
    }
}

/// Runtime facts sampled by the Kernel while it holds the same admission lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeUsage {
    /// Whether the Kernel could query all Worker/Lease/allocation owners.
    pub known: bool,
    pub active_worker_count: u64,
    pub active_allocation_count: u64,
}

/// Update preview parameters. The expected source list is compared with the full
/// trusted catalog and can never narrow the service-side authority set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessRequest {
    pub target_kind: UpdateTargetKind,
    pub requires_restart: bool,
    pub expected_catalog_generation: u64,
    pub expected_activity_sources: Vec<String>,
}

/// Update readiness snapshot suitable for UI and updater reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessSnapshot {
    pub status: ReadinessStatus,
    pub gate_generation: u64,
    pub install_catalog_generation: u64,
    pub active_task_count: u64,
    pub active_tasks: Vec<TaskActivityRecord>,
    pub inflight_runtime_admission_count: u64,
    pub active_binding_operation_count: u64,
    pub active_binding_operations: Vec<BindingOperationActivity>,
    pub unknown_activity_sources: Vec<String>,
    pub active_worker_count: u64,
    pub active_allocation_count: u64,
    pub blocker_codes: Vec<String>,
    pub requires_restart_confirmation: bool,
}

/// One active task record included in readiness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskActivityRecord {
    pub source_id: String,
    pub task_id: String,
    pub state: TaskActivityState,
}

/// Result returned when a task source has acquired a durable admission record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAdmission {
    pub token: String,
    pub source_id: String,
    pub task_id: String,
    pub state: TaskActivityState,
    pub gate_generation: u64,
}

/// Durable outcome recorded by an updater after apply or rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MaintenanceOutcome {
    Success,
    RolledBack,
    Failed,
}

/// Result returned by an attempted maintenance transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeginMaintenanceResult {
    pub status: ReadinessStatus,
    pub maintenance_token: Option<String>,
    pub gate_generation: u64,
    pub blocker_codes: Vec<String>,
}

/// Immutable plan identity and artifact digests bound to an operator-confirmed apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenancePlan {
    pub plan_id: String,
    pub plan_digest: String,
    pub component_artifact_digests: BTreeMap<String, String>,
}

/// Private proof that a catalog update belongs to the current maintenance hold.
#[derive(Clone, PartialEq, Eq)]
pub struct MaintenanceHoldProof {
    pub request_id: String,
    pub maintenance_token: String,
    pub plan: MaintenancePlan,
}

/// Identifies one explicitly supported schema-1 state layout for offline migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyStateProfile {
    /// Released schema 1 had neither binding-operation map.
    ReleasedV1NoBindingAdmissions,
    /// The experimental schema-1 writer persisted both binding-operation maps.
    ExperimentalV1BindingAdmissions,
}

/// Private root-generated proof used by the offline schema migration commands.
///
/// The CLI reads this value only from a root-owned mode-0600 file. Its token is
/// never serialized into the migration marker or normal logs.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMigrationProof {
    pub schema1_profile: LegacyStateProfile,
    pub request_id: String,
    pub maintenance_token: String,
    pub target_kind: UpdateTargetKind,
    pub plan_id: String,
    pub plan_digest: String,
    pub component_artifact_digests: BTreeMap<String, String>,
    pub expected_gate_generation: u64,
    pub expected_catalog_generation: u64,
}

/// Result of completing or resuming the controlled schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMigrationResult {
    pub migrated: bool,
    pub schema_version: u32,
    pub migration_id: String,
    pub gate_generation: u64,
    pub catalog_generation: u64,
}

/// Read-only confirmation of the exact active maintenance hold and generations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceHoldValidation {
    pub valid: bool,
    pub request_id: String,
    pub target_kind: UpdateTargetKind,
    #[serde(flatten)]
    pub plan: MaintenancePlan,
    pub component_id: String,
    pub artifact_digest: String,
    pub gate_generation: u64,
    pub catalog_generation: u64,
}

/// Result returned by a maintenance completion attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndMaintenanceResult {
    pub unlocked: bool,
    pub status: ReadinessStatus,
    pub gate_generation: u64,
}

/// Errors are intentionally fail-closed and include stable machine-readable codes.
#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("trusted activity-source catalog is unavailable: {0}")]
    CatalogUnavailable(String),
    #[error("maintenance state is unknown: {0}")]
    StateUnknown(String),
    #[error("maintenance storage I/O failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error("maintenance serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("admission denied: {0}")]
    AdmissionDenied(String),
}

impl MaintenanceError {
    /// Stable error code exposed over Kernel RPC and JSON IPC.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "INVALID_ARGUMENT",
            Self::CatalogUnavailable(_) | Self::StateUnknown(_) => "UPDATE_READINESS_UNKNOWN",
            Self::Storage(_) | Self::Serialization(_) => "MAINTENANCE_STORAGE_UNAVAILABLE",
            Self::AdmissionDenied(_) => "MAINTENANCE_ADMISSION_DENIED",
        }
    }
}

#[derive(Clone)]
pub struct RuntimeMaintenance {
    inner: Arc<Inner>,
}

struct Inner {
    directory: PathBuf,
    catalog: RwLock<TrustedActivitySourceCatalog>,
    catalog_path: Option<PathBuf>,
    source_staleness: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedState {
    schema_version: u32,
    journal_sequence: u64,
    gate_generation: u64,
    install_catalog_generation: u64,
    maintenance: Option<MaintenanceRecord>,
    completed_maintenances: BTreeMap<String, CompletedMaintenanceRecord>,
    tasks: BTreeMap<String, TaskRecord>,
    runtime_admissions: BTreeMap<String, String>,
    #[serde(default)]
    binding_operations: BTreeMap<String, BindingOperationRecord>,
    #[serde(default)]
    completed_binding_operations: BTreeMap<String, BindingOperationRecord>,
    sources: BTreeMap<String, SourceRecord>,
    /// True only while the journal contains catalog setup and no prior runtime use.
    #[serde(default)]
    core_bootstrap_eligible: bool,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            journal_sequence: 0,
            gate_generation: 0,
            install_catalog_generation: 0,
            maintenance: None,
            completed_maintenances: BTreeMap::new(),
            tasks: BTreeMap::new(),
            runtime_admissions: BTreeMap::new(),
            binding_operations: BTreeMap::new(),
            completed_binding_operations: BTreeMap::new(),
            sources: BTreeMap::new(),
            core_bootstrap_eligible: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceRecord {
    request_id: String,
    token: String,
    target_kind: UpdateTargetKind,
    requires_restart: bool,
    user_confirmed_restart: bool,
    expected_gate_generation: u64,
    expected_catalog_generation: u64,
    expected_activity_sources: Vec<String>,
    plan: MaintenancePlan,
    started_at_unix_ms: u64,
    /// Identifies the narrowly scoped first-Kernel bootstrap hold.
    #[serde(default)]
    origin: MaintenanceOrigin,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum MaintenanceOrigin {
    #[default]
    Standard,
    CoreBootstrap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletedMaintenanceRecord {
    maintenance: MaintenanceRecord,
    outcome: MaintenanceOutcome,
    healthy: bool,
    unlocked: bool,
    ended_at_unix_ms: u64,
    gate_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum StateMigrationPhase {
    Prepared,
    JournalReplaced,
    SnapshotReplaced,
    Complete,
    RollbackPrepared,
    RollbackJournalReplaced,
    RollbackSnapshotReplaced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateMigrationMarker {
    migration_version: u32,
    migration_id: String,
    from_schema_version: u32,
    to_schema_version: u32,
    phase: StateMigrationPhase,
    schema1_profile: LegacyStateProfile,
    request_id: String,
    maintenance_token_sha256: String,
    target_kind: UpdateTargetKind,
    plan_id: String,
    plan_digest: String,
    component_artifact_digests: BTreeMap<String, String>,
    expected_gate_generation: u64,
    expected_catalog_generation: u64,
    source_state_sha256: String,
    source_journal_sha256: String,
    target_state_sha256: String,
    target_journal_sha256: String,
    backup_state_file: String,
    backup_journal_file: String,
    backup_schema2_state_file: String,
    backup_schema2_journal_file: String,
    rollback_from_state_sha256: Option<String>,
    rollback_from_journal_sha256: Option<String>,
    rollback_target_state_sha256: Option<String>,
    rollback_target_journal_sha256: Option<String>,
    rollback_target_state_file: Option<String>,
    rollback_target_journal_file: Option<String>,
}

impl StateMigrationMarker {
    fn backup_state_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.backup_state_file)
    }

    fn backup_journal_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.backup_journal_file)
    }

    fn backup_schema2_state_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.backup_schema2_state_file)
    }

    fn backup_schema2_journal_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.backup_schema2_journal_file)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRecord {
    admission: TaskAdmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    last_heartbeat_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingOperationRecord {
    request_id: String,
    operation_token: String,
    source_id: String,
    scope: BindingOperationScope,
    started_at_unix_ms: u64,
}

impl BindingOperationRecord {
    fn admission(
        &self,
        catalog_generation: u64,
        gate_generation: u64,
        already_in_flight: bool,
        already_completed: bool,
    ) -> BindingOperationAdmission {
        BindingOperationAdmission {
            request_id: self.request_id.clone(),
            source_id: self.source_id.clone(),
            scope: self.scope.clone(),
            operation_token: self.operation_token.clone(),
            catalog_generation,
            gate_generation,
            already_in_flight,
            already_completed,
        }
    }

    fn activity(&self) -> BindingOperationActivity {
        BindingOperationActivity {
            request_id: self.request_id.clone(),
            source_id: self.source_id.clone(),
            scope: self.scope.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    sequence: u64,
    event: JournalEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum JournalEvent {
    CatalogConfigured {
        generation: u64,
    },
    SourceHeartbeat {
        source_id: String,
        at_unix_ms: u64,
    },
    TaskAdmitted {
        admission: TaskAdmission,
    },
    TaskUpdated {
        admission: TaskAdmission,
    },
    TaskCompleted {
        source_id: String,
        task_id: String,
    },
    TasksReconciled {
        source_id: String,
        active_tasks: Vec<TaskAdmission>,
        at_unix_ms: u64,
    },
    StateCheckpoint {
        state: Box<PersistedState>,
    },
    RuntimeAdmissionStarted {
        token: String,
        action: String,
    },
    RuntimeAdmissionEnded {
        token: String,
    },
    RuntimeAdmissionsRecovered,
    BindingOperationAdmitted {
        record: BindingOperationRecord,
    },
    BindingOperationCompleted {
        record: BindingOperationRecord,
    },
    MaintenanceBegan {
        record: MaintenanceRecord,
    },
    MaintenanceEnded {
        request_id: String,
        token: String,
        outcome: MaintenanceOutcome,
        healthy: bool,
        unlocked: bool,
        ended_at_unix_ms: u64,
    },
}

impl RuntimeMaintenance {
    /// Opens the shared root-owned state directory and recovers its journal.
    pub fn open(
        directory: impl Into<PathBuf>,
        catalog: TrustedActivitySourceCatalog,
    ) -> Result<Self, MaintenanceError> {
        Self::open_inner(directory.into(), catalog, None, false, true)
    }

    /// Opens shared state with a root-managed catalog that is reloaded under
    /// the same file lock before each operation. This lets Products start or
    /// stop independently without requiring a Kernel restart to refresh the
    /// installed-source set.
    pub fn open_with_catalog_file(
        directory: impl Into<PathBuf>,
        catalog_path: impl Into<PathBuf>,
    ) -> Result<Self, MaintenanceError> {
        let catalog_path = catalog_path.into();
        let catalog = TrustedActivitySourceCatalog::load(&catalog_path)?;
        Self::open_inner(directory.into(), catalog, Some(catalog_path), false, true)
    }

    /// Opens shared state like [`Self::open_with_catalog_file`], tolerating a
    /// catalog file that does not exist yet as the first-boot unprovisioned
    /// state instead of failing startup.
    ///
    /// The unprovisioned broker stays alive but fails closed: its catalog
    /// generation is zero, no activity source is trusted, and every admission
    /// or readiness request keeps failing until `init-catalog` provisions the
    /// file, after which the per-operation reload adopts it. A catalog that
    /// exists but is corrupt or has weak permissions still fails startup, and
    /// deleting a catalog after it was provisioned (state generation above
    /// zero) is refused rather than treated as unprovisioned.
    pub fn open_with_optional_catalog_file(
        directory: impl Into<PathBuf>,
        catalog_path: impl Into<PathBuf>,
    ) -> Result<Self, MaintenanceError> {
        let catalog_path = catalog_path.into();
        // Only a genuinely absent catalog file is the first-boot unprovisioned
        // state. Anything unclassifiable (dangling symlink, permission error,
        // ...) must stay fail-closed instead of being treated as absent.
        let provisioned = match fs::symlink_metadata(&catalog_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(MaintenanceError::CatalogUnavailable(format!(
                    "{}: {error}",
                    catalog_path.display()
                )))
            }
            Ok(_) => true,
        };
        let catalog = if provisioned {
            TrustedActivitySourceCatalog::load(&catalog_path)?
        } else {
            TrustedActivitySourceCatalog::unprovisioned()
        };
        Self::open_inner(
            directory.into(),
            catalog,
            Some(catalog_path),
            false,
            provisioned,
        )
    }

    /// Opens isolated test state owned by the test process without weakening
    /// production callers' root-managed shared-directory requirement.
    #[cfg(feature = "test-utils")]
    pub fn open_for_test(
        directory: impl Into<PathBuf>,
        catalog: TrustedActivitySourceCatalog,
    ) -> Result<Self, MaintenanceError> {
        Self::open_inner(directory.into(), catalog, None, true, true)
    }

    fn open_inner(
        directory: PathBuf,
        catalog: TrustedActivitySourceCatalog,
        catalog_path: Option<PathBuf>,
        allow_test_owner: bool,
        provisioned: bool,
    ) -> Result<Self, MaintenanceError> {
        if provisioned {
            catalog.validate()?;
        }
        if !directory.exists() {
            fs::create_dir_all(&directory)?;
        }
        reject_symlink_directory(&directory)?;
        set_shared_directory(&directory, allow_test_owner)?;
        validate_shared_files(&directory)?;
        ensure_migration_marker_allows_state2(&directory)?;
        let this = Self {
            inner: Arc::new(Inner {
                directory,
                catalog: RwLock::new(catalog),
                catalog_path,
                source_staleness: DEFAULT_SOURCE_STALENESS,
            }),
        };
        this.with_state(|_| Ok(()))?;
        Ok(this)
    }

    /// Reloads a dynamic root-owned catalog and publishes its generation before
    /// serving the next admission/readiness operation.
    pub fn refresh_catalog(&self) -> Result<(), MaintenanceError> {
        self.with_state(|_| Ok(()))
    }

    /// Commits a root-managed catalog replacement while holding the same lock as
    /// operation admission and completion. The writer runs only when the expected
    /// catalog is current, no binding operation is active, and any maintenance
    /// hold has the exact transaction, token, and plan proof.
    pub fn commit_activity_source_catalog(
        &self,
        expected_generation: u64,
        catalog: TrustedActivitySourceCatalog,
        maintenance_proof: Option<&MaintenanceHoldProof>,
        write_catalog: impl FnOnce() -> Result<(), MaintenanceError>,
    ) -> Result<(), MaintenanceError> {
        catalog.validate()?;
        let next_generation = expected_generation.checked_add(1).ok_or_else(|| {
            MaintenanceError::InvalidRequest("activity catalog generation exhausted".to_string())
        })?;
        if catalog.generation != next_generation {
            return Err(MaintenanceError::InvalidRequest(
                "replacement catalog generation must advance exactly once".to_string(),
            ));
        }

        let lock = open_lock_file(&self.inner.directory)?;
        lock.lock()?;
        let mut state = load_state_locked(&self.inner.directory)?;
        let result = (|| {
            self.refresh_catalog_locked(&mut state)?;
            let current = self.catalog_snapshot()?;
            if current.generation != expected_generation
                || state.install_catalog_generation != expected_generation
            {
                return Err(MaintenanceError::CatalogUnavailable(
                    "activity catalog changed while replacement was being prepared".to_string(),
                ));
            }
            if !state.binding_operations.is_empty() {
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATIONS_INFLIGHT".to_string(),
                ));
            }
            match (state.maintenance.as_ref(), maintenance_proof) {
                (None, None) => {}
                (None, Some(_)) => {
                    return Err(MaintenanceError::AdmissionDenied(
                        "MAINTENANCE_NOT_ACTIVE".to_string(),
                    ));
                }
                (Some(_), None) => {
                    return Err(MaintenanceError::AdmissionDenied(
                        "MAINTENANCE_HOLD_PROOF_REQUIRED".to_string(),
                    ));
                }
                (Some(active), Some(proof))
                    if active.request_id == proof.request_id
                        && active.token == proof.maintenance_token
                        && active.plan == proof.plan =>
                {
                    // The exact transaction owner may update the catalog under its hold.
                }
                (Some(_), Some(_)) => {
                    return Err(MaintenanceError::AdmissionDenied(
                        "MAINTENANCE_HOLD_PROOF_MISMATCH".to_string(),
                    ));
                }
            }

            write_catalog()?;
            append_and_apply(
                &self.inner.directory,
                &mut state,
                JournalEvent::CatalogConfigured {
                    generation: catalog.generation,
                },
            )?;
            let mut current = self.inner.catalog.write().map_err(|_| {
                MaintenanceError::StateUnknown("trusted catalog lock poisoned".into())
            })?;
            *current = catalog;
            Ok(())
        })();
        let unlock_result = lock.unlock();
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(MaintenanceError::Storage(error)),
        }
    }

    /// Adjusts the source freshness window for a deployment or deterministic test.
    pub fn with_source_staleness(mut self, staleness: Duration) -> Result<Self, MaintenanceError> {
        if staleness.is_zero() {
            return Err(MaintenanceError::InvalidRequest(
                "source staleness must be positive".to_string(),
            ));
        }
        Arc::get_mut(&mut self.inner)
            .expect("fresh RuntimeMaintenance cannot already be shared")
            .source_staleness = staleness;
        Ok(self)
    }

    /// Returns the complete installed-source set for catalog consistency checks.
    pub fn trusted_source_ids(&self) -> Vec<String> {
        self.inner
            .catalog
            .read()
            .map(|catalog| catalog.source_ids().into_iter().collect())
            .unwrap_or_default()
    }

    /// Returns the generation of the root-owned installed-source catalog.
    pub fn catalog_generation(&self) -> u64 {
        self.inner
            .catalog
            .read()
            .map(|catalog| catalog.generation)
            .unwrap_or_default()
    }

    /// Returns the current readiness generation under the shared file lock.
    pub fn current_gate_generation(&self) -> Result<u64, MaintenanceError> {
        self.with_state(|state| Ok(state.gate_generation))
    }

    /// Returns the persisted first-CoreRuntime bootstrap eligibility under the shared gate lock.
    ///
    /// This is only a freshness fact. It does not claim readiness, runtime idleness,
    /// or the absence of external legacy processes and compute resources.
    pub fn core_bootstrap_eligible(&self) -> Result<bool, MaintenanceError> {
        self.with_state(|state| Ok(state.core_bootstrap_eligible))
    }

    /// Returns the source's configured local IPC UID/GID identity.
    pub fn trusted_source(&self, source_id: &str) -> Option<TrustedActivitySource> {
        self.inner
            .catalog
            .read()
            .ok()
            .and_then(|catalog| catalog.source(source_id).cloned())
    }

    /// Verifies a per-source secret without exposing catalog credentials to callers.
    pub fn verify_source_token(&self, source_id: &str, token: &str) -> bool {
        use sha2::{Digest, Sha256};
        let Some(source) = self.trusted_source(source_id) else {
            return false;
        };
        let expected = source.source_token_sha256.as_bytes();
        let actual = format!("{:x}", Sha256::digest(token.as_bytes()));
        constant_time_eq(expected, actual.as_bytes())
    }

    /// Verifies the operator capability stored only in the broker's private state volume.
    pub fn verify_operator_token(&self, token: &str) -> Result<bool, MaintenanceError> {
        let expected = fs::read_to_string(self.inner.directory.join(OPERATOR_TOKEN_HASH_FILE))?;
        let actual = format!("{:x}", Sha256::digest(token.as_bytes()));
        Ok(constant_time_eq(
            expected.trim().as_bytes(),
            actual.as_bytes(),
        ))
    }

    /// Initializes or reads the private operator capability and publishes only its hash.
    pub fn initialize_operator_capability(
        &self,
        private_token_path: impl AsRef<Path>,
    ) -> Result<String, MaintenanceError> {
        let private_token_path = private_token_path.as_ref();
        ensure_secret_file(private_token_path)?;
        let token = fs::read_to_string(private_token_path)?.trim().to_string();
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        write_private_hash_atomic(
            &self.inner.directory.join(OPERATOR_TOKEN_HASH_FILE),
            hash.as_bytes(),
        )?;
        Ok(token)
    }

    /// Records a fresh authenticated source heartbeat.
    pub fn heartbeat_activity_source(&self, source_id: &str) -> Result<(), MaintenanceError> {
        self.require_trusted_source(source_id)?;
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::SourceHeartbeat {
                    source_id: source_id.to_string(),
                    at_unix_ms: now_unix_ms(),
                },
            )
        })
    }

    /// Durably reserves a task before its Product source accepts or queues it.
    pub fn admit_task(
        &self,
        source_id: &str,
        task_id: &str,
        activity_state: TaskActivityState,
    ) -> Result<TaskAdmission, MaintenanceError> {
        self.require_trusted_source(source_id)?;
        validate_identifier(task_id, "task_id")?;
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            if state.maintenance.is_some() {
                return Err(MaintenanceError::AdmissionDenied(
                    "UPDATE_MAINTENANCE_ACTIVE".to_string(),
                ));
            }
            require_fresh_source(state, source_id, self.inner.source_staleness)?;
            let key = task_key(source_id, task_id);
            if let Some(existing) = state.tasks.get(&key) {
                let mut admission = existing.admission.clone();
                admission.state = activity_state;
                if admission.state != existing.admission.state {
                    append_and_apply(
                        &self.inner.directory,
                        state,
                        JournalEvent::TaskUpdated {
                            admission: admission.clone(),
                        },
                    )?;
                }
                return Ok(admission);
            }
            let admission = TaskAdmission {
                token: Uuid::new_v4().to_string(),
                source_id: source_id.to_string(),
                task_id: task_id.to_string(),
                state: activity_state,
                gate_generation: state.gate_generation.saturating_add(1),
            };
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::TaskAdmitted {
                    admission: admission.clone(),
                },
            )?;
            Ok(admission)
        })
    }

    /// Advances one already admitted task through its nonterminal lifecycle.
    pub fn update_task_activity(
        &self,
        source_id: &str,
        task_id: &str,
        activity_state: TaskActivityState,
    ) -> Result<TaskAdmission, MaintenanceError> {
        self.require_trusted_source(source_id)?;
        let key = task_key(source_id, task_id);
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            let mut admission = state
                .tasks
                .get(&key)
                .ok_or_else(|| {
                    MaintenanceError::AdmissionDenied("TASK_ADMISSION_NOT_FOUND".into())
                })?
                .admission
                .clone();
            admission.state = activity_state;
            admission.gate_generation = state.gate_generation.saturating_add(1);
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::TaskUpdated {
                    admission: admission.clone(),
                },
            )?;
            Ok(admission)
        })
    }

    /// Removes an activity record after the source durably enters a terminal state.
    pub fn complete_task(&self, source_id: &str, task_id: &str) -> Result<(), MaintenanceError> {
        self.require_trusted_source(source_id)?;
        validate_identifier(task_id, "task_id")?;
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            if !state.tasks.contains_key(&task_key(source_id, task_id)) {
                return Ok(());
            }
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::TaskCompleted {
                    source_id: source_id.to_string(),
                    task_id: task_id.to_string(),
                },
            )
        })
    }

    /// Durably admits one exact package binding operation before the daemon mutates runtime state.
    ///
    /// Catalog generation, source token, peer UID/GID, exact binding scope, and
    /// maintenance state are checked while holding the shared file lock. The
    /// reservation survives process exit until the daemon completes this exact
    /// request; no age-based cleanup can make an unknown operation look idle.
    pub fn admit_binding_operation(
        &self,
        request_id: &str,
        caller: &BindingOperationCaller,
        scope: &BindingOperationScope,
    ) -> Result<BindingOperationAdmission, MaintenanceError> {
        validate_identifier(request_id, "request_id")?;
        validate_binding_operation_scope(scope)?;
        validate_identifier(&caller.source_id, "source_id")?;
        if caller.source_token.is_empty() || caller.source_token.len() > 4096 {
            return Err(MaintenanceError::InvalidRequest(
                "source_token must be non-empty and at most 4096 bytes".to_string(),
            ));
        }
        self.with_state(|state| {
            self.require_binding_operation_authority(state, caller, scope)?;

            let candidate = BindingOperationRecord {
                request_id: request_id.to_string(),
                operation_token: String::new(),
                source_id: caller.source_id.clone(),
                scope: scope.clone(),
                started_at_unix_ms: 0,
            };
            if let Some(existing) = state.binding_operations.get(request_id) {
                if same_binding_operation(existing, &candidate) {
                    if state.maintenance.is_some() {
                        return Err(MaintenanceError::AdmissionDenied(
                            "UPDATE_MAINTENANCE_ACTIVE".to_string(),
                        ));
                    }
                    return Ok(existing.admission(
                        state.install_catalog_generation,
                        state.gate_generation,
                        true,
                        false,
                    ));
                }
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_REQUEST_ID_CONFLICT".to_string(),
                ));
            }
            if let Some(existing) = state.completed_binding_operations.get(request_id) {
                if same_binding_operation(existing, &candidate) {
                    return Ok(existing.admission(
                        state.install_catalog_generation,
                        state.gate_generation,
                        false,
                        true,
                    ));
                }
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_REQUEST_ID_CONFLICT".to_string(),
                ));
            }
            if state
                .binding_operations
                .values()
                .any(|active| active.scope.binding_id == scope.binding_id)
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_ALREADY_INFLIGHT".to_string(),
                ));
            }
            if state.maintenance.is_some() {
                return Err(MaintenanceError::AdmissionDenied(
                    "UPDATE_MAINTENANCE_ACTIVE".to_string(),
                ));
            }

            let record = BindingOperationRecord {
                request_id: request_id.to_string(),
                operation_token: Uuid::new_v4().to_string(),
                source_id: caller.source_id.clone(),
                scope: scope.clone(),
                started_at_unix_ms: now_unix_ms(),
            };
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::BindingOperationAdmitted {
                    record: record.clone(),
                },
            )?;
            Ok(record.admission(
                state.install_catalog_generation,
                state.gate_generation,
                false,
                false,
            ))
        })
    }

    /// Completes only the same authenticated owner, request, and exact operation scope.
    ///
    /// Replays of an already completed identical request are idempotent. A
    /// mismatched token, owner, binding, package, installation, or operation is
    /// rejected and leaves the durable blocker intact.
    pub fn complete_binding_operation(
        &self,
        request_id: &str,
        caller: &BindingOperationCaller,
        scope: &BindingOperationScope,
        operation_token: &str,
    ) -> Result<BindingOperationCompletion, MaintenanceError> {
        validate_identifier(request_id, "request_id")?;
        validate_binding_operation_scope(scope)?;
        validate_identifier(&caller.source_id, "source_id")?;
        if caller.source_token.is_empty() || caller.source_token.len() > 4096 {
            return Err(MaintenanceError::InvalidRequest(
                "source_token must be non-empty and at most 4096 bytes".to_string(),
            ));
        }
        if operation_token.is_empty() || operation_token.len() > 256 {
            return Err(MaintenanceError::InvalidRequest(
                "operation_token must be non-empty and at most 256 bytes".to_string(),
            ));
        }
        self.with_state(|state| {
            self.require_binding_operation_authority(state, caller, scope)?;
            let expected = BindingOperationRecord {
                request_id: request_id.to_string(),
                operation_token: operation_token.to_string(),
                source_id: caller.source_id.clone(),
                scope: scope.clone(),
                started_at_unix_ms: 0,
            };
            if let Some(completed) = state.completed_binding_operations.get(request_id) {
                if same_binding_operation(completed, &expected)
                    && completed.operation_token == operation_token
                {
                    return Ok(BindingOperationCompletion {
                        completed: true,
                        gate_generation: state.gate_generation,
                    });
                }
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_COMPLETION_SCOPE_MISMATCH".to_string(),
                ));
            }
            let Some(active) = state.binding_operations.get(request_id) else {
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_NOT_FOUND".to_string(),
                ));
            };
            if !same_binding_operation(active, &expected)
                || active.operation_token != operation_token
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "BINDING_OPERATION_COMPLETION_SCOPE_MISMATCH".to_string(),
                ));
            }
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::BindingOperationCompleted {
                    record: active.clone(),
                },
            )?;
            Ok(BindingOperationCompletion {
                completed: true,
                gate_generation: state.gate_generation,
            })
        })
    }

    fn require_binding_operation_authority(
        &self,
        state: &PersistedState,
        caller: &BindingOperationCaller,
        scope: &BindingOperationScope,
    ) -> Result<(), MaintenanceError> {
        let catalog = self.catalog_snapshot()?;
        if caller.expected_catalog_generation != catalog.generation
            || caller.expected_catalog_generation != state.install_catalog_generation
        {
            return Err(MaintenanceError::AdmissionDenied(
                "ACTIVITY_CATALOG_GENERATION_MISMATCH".to_string(),
            ));
        }
        let source = catalog.source(&caller.source_id).ok_or_else(|| {
            MaintenanceError::AdmissionDenied("ACTIVITY_SOURCE_UNTRUSTED".to_string())
        })?;
        if source.uid != caller.peer_uid || source.gid.is_some_and(|gid| gid != caller.peer_gid) {
            return Err(MaintenanceError::AdmissionDenied(
                "ACTIVITY_SOURCE_CALLER_MISMATCH".to_string(),
            ));
        }
        let expected_token_hash = source.source_token_sha256.as_bytes();
        let actual_token_hash = format!("{:x}", Sha256::digest(caller.source_token.as_bytes()));
        if !constant_time_eq(expected_token_hash, actual_token_hash.as_bytes()) {
            return Err(MaintenanceError::AdmissionDenied(
                "ACTIVITY_SOURCE_AUTH_INVALID".to_string(),
            ));
        }
        let authorized = source.binding_scopes.iter().any(|binding_scope| {
            binding_scope.binding_id == scope.binding_id
                && binding_scope.package_id == scope.package_id
                && binding_scope
                    .installation_ids
                    .iter()
                    .any(|id| id == &scope.installation_id)
                && binding_scope.operations.contains(&scope.operation)
        });
        if !authorized {
            return Err(MaintenanceError::AdmissionDenied(
                "BINDING_OPERATION_SCOPE_UNTRUSTED".to_string(),
            ));
        }
        Ok(())
    }

    /// Lists the durable active tasks for one trusted source before startup reconciliation.
    pub fn list_active_tasks(
        &self,
        source_id: &str,
    ) -> Result<Vec<TaskActivityRecord>, MaintenanceError> {
        self.require_trusted_source(source_id)?;
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            Ok(active_tasks_for(state, Some(source_id)))
        })
    }

    /// Reconciles one source's durable task set against its own business database.
    /// Existing records can be repaired while maintenance is active, but a locked
    /// gate cannot gain a new task that would invalidate the maintenance decision.
    pub fn reconcile_activity_source(
        &self,
        source_id: &str,
        active_tasks: Vec<TaskActivityRecord>,
    ) -> Result<(), MaintenanceError> {
        self.require_trusted_source(source_id)?;
        for task in &active_tasks {
            if task.source_id != source_id {
                return Err(MaintenanceError::InvalidRequest(
                    "reconciliation contains a task owned by another source".to_string(),
                ));
            }
            validate_identifier(&task.task_id, "task_id")?;
        }
        let mut seen = BTreeSet::new();
        if active_tasks
            .iter()
            .any(|task| !seen.insert(task.task_id.as_str()))
        {
            return Err(MaintenanceError::InvalidRequest(
                "reconciliation contains duplicate task ids".to_string(),
            ));
        }
        self.with_state(|state| {
            self.require_trusted_source_current(source_id)?;
            if state.maintenance.is_some()
                && active_tasks.iter().any(|task| {
                    !state
                        .tasks
                        .contains_key(&task_key(source_id, &task.task_id))
                })
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "UPDATE_MAINTENANCE_ACTIVE".to_string(),
                ));
            }
            let generation = state.gate_generation.saturating_add(1);
            let admissions = active_tasks
                .into_iter()
                .map(|task| {
                    state
                        .tasks
                        .get(&task_key(source_id, &task.task_id))
                        .map(|record| {
                            let mut admission = record.admission.clone();
                            admission.state = task.state;
                            admission.gate_generation = generation;
                            admission
                        })
                        .unwrap_or_else(|| TaskAdmission {
                            token: Uuid::new_v4().to_string(),
                            source_id: source_id.to_string(),
                            task_id: task.task_id,
                            state: task.state,
                            gate_generation: generation,
                        })
                })
                .collect();
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::TasksReconciled {
                    source_id: source_id.to_string(),
                    active_tasks: admissions,
                    at_unix_ms: now_unix_ms(),
                },
            )
        })
    }

    /// Computes readiness against the full trusted install catalog and live Kernel facts.
    pub fn get_update_readiness(
        &self,
        request: &ReadinessRequest,
        runtime_usage: RuntimeUsage,
    ) -> Result<ReadinessSnapshot, MaintenanceError> {
        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            Ok(readiness_snapshot(
                state,
                &catalog,
                self.inner.source_staleness,
                request,
                runtime_usage,
            ))
        })
    }

    /// Computes readiness while holding the shared gate lock before sampling live runtime facts.
    pub fn get_update_readiness_with(
        &self,
        request: &ReadinessRequest,
        runtime_usage: impl FnOnce() -> RuntimeUsage,
    ) -> Result<ReadinessSnapshot, MaintenanceError> {
        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            // CoreRuntime callers need live counts even when another blocker
            // is UNKNOWN; zero-valued defaults must never look like proof of idle.
            let runtime_usage = if request.target_kind == UpdateTargetKind::CoreRuntime {
                runtime_usage()
            } else {
                RuntimeUsage::default()
            };
            Ok(readiness_snapshot(
                state,
                &catalog,
                self.inner.source_staleness,
                request,
                runtime_usage,
            ))
        })
    }

    /// Atomically rechecks readiness and persists a maintenance token before apply.
    pub fn begin_maintenance(
        &self,
        request_id: &str,
        plan: &MaintenancePlan,
        request: &ReadinessRequest,
        expected_gate_generation: u64,
        user_confirmed_restart: bool,
        runtime_usage: RuntimeUsage,
    ) -> Result<BeginMaintenanceResult, MaintenanceError> {
        validate_identifier(request_id, "request_id")?;
        validate_plan(plan)?;
        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            if let Some(existing) = &state.maintenance {
                if existing.request_id == request_id {
                    let expected_sources = request
                        .expected_activity_sources
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    if existing.origin != MaintenanceOrigin::Standard
                        || &existing.plan != plan
                        || existing.target_kind != request.target_kind
                        || existing.requires_restart != request.requires_restart
                        || existing.user_confirmed_restart != user_confirmed_restart
                        || existing.expected_catalog_generation
                            != request.expected_catalog_generation
                        || existing.expected_activity_sources != expected_sources
                    {
                        return Err(MaintenanceError::AdmissionDenied(
                            "REQUEST_ID_PLAN_MISMATCH".to_string(),
                        ));
                    }
                    return Ok(BeginMaintenanceResult {
                        status: ReadinessStatus::MaintenanceActive,
                        maintenance_token: Some(existing.token.clone()),
                        gate_generation: state.gate_generation,
                        blocker_codes: vec!["MAINTENANCE_ALREADY_BEGUN".to_string()],
                    });
                }
            }
            if state.completed_maintenances.contains_key(request_id) {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_REQUEST_ALREADY_COMPLETED".to_string(),
                ));
            }
            let snapshot = readiness_snapshot(
                state,
                &catalog,
                self.inner.source_staleness,
                request,
                runtime_usage,
            );
            if snapshot.gate_generation != expected_gate_generation {
                return Ok(BeginMaintenanceResult {
                    status: ReadinessStatus::StaleReadiness,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: vec!["READINESS_GENERATION_STALE".to_string()],
                });
            }
            if snapshot.status != ReadinessStatus::Ready {
                return Ok(BeginMaintenanceResult {
                    status: snapshot.status,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: snapshot.blocker_codes,
                });
            }
            if request.requires_restart && !user_confirmed_restart {
                return Ok(BeginMaintenanceResult {
                    status: ReadinessStatus::UserConfirmationRequired,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: vec!["USER_CONFIRMATION_REQUIRED".to_string()],
                });
            }
            let record = MaintenanceRecord {
                request_id: request_id.to_string(),
                token: Uuid::new_v4().to_string(),
                target_kind: request.target_kind,
                requires_restart: request.requires_restart,
                user_confirmed_restart,
                expected_gate_generation: snapshot.gate_generation,
                expected_catalog_generation: request.expected_catalog_generation,
                expected_activity_sources: request
                    .expected_activity_sources
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                plan: plan.clone(),
                started_at_unix_ms: now_unix_ms(),
                origin: MaintenanceOrigin::Standard,
            };
            let token = record.token.clone();
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::MaintenanceBegan { record },
            )?;
            Ok(BeginMaintenanceResult {
                status: ReadinessStatus::MaintenanceActive,
                maintenance_token: Some(token),
                gate_generation: state.gate_generation,
                blocker_codes: Vec::new(),
            })
        })
    }

    /// Rechecks and begins maintenance while holding the gate lock around runtime sampling.
    pub fn begin_maintenance_with(
        &self,
        request_id: &str,
        plan: &MaintenancePlan,
        request: &ReadinessRequest,
        expected_gate_generation: u64,
        user_confirmed_restart: bool,
        runtime_usage: impl FnOnce() -> RuntimeUsage,
    ) -> Result<BeginMaintenanceResult, MaintenanceError> {
        validate_identifier(request_id, "request_id")?;
        validate_plan(plan)?;
        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            if let Some(existing) = &state.maintenance {
                if existing.request_id == request_id {
                    let expected_sources = request
                        .expected_activity_sources
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    if existing.origin != MaintenanceOrigin::Standard
                        || &existing.plan != plan
                        || existing.target_kind != request.target_kind
                        || existing.requires_restart != request.requires_restart
                        || existing.user_confirmed_restart != user_confirmed_restart
                        || existing.expected_catalog_generation
                            != request.expected_catalog_generation
                        || existing.expected_activity_sources != expected_sources
                    {
                        return Err(MaintenanceError::AdmissionDenied(
                            "REQUEST_ID_PLAN_MISMATCH".to_string(),
                        ));
                    }
                    return Ok(BeginMaintenanceResult {
                        status: ReadinessStatus::MaintenanceActive,
                        maintenance_token: Some(existing.token.clone()),
                        gate_generation: state.gate_generation,
                        blocker_codes: vec!["MAINTENANCE_ALREADY_BEGUN".to_string()],
                    });
                }
            }
            if state.completed_maintenances.contains_key(request_id) {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_REQUEST_ALREADY_COMPLETED".to_string(),
                ));
            }
            let preliminary = readiness_snapshot(
                state,
                &catalog,
                self.inner.source_staleness,
                request,
                RuntimeUsage::default(),
            );
            let needs_runtime_facts = preliminary.status == ReadinessStatus::Unknown
                && preliminary.blocker_codes.len() == 1
                && preliminary.blocker_codes[0] == "RUNTIME_ACTIVITY_UNKNOWN";
            if preliminary.status != ReadinessStatus::Ready && !needs_runtime_facts {
                return Ok(BeginMaintenanceResult {
                    status: preliminary.status,
                    maintenance_token: None,
                    gate_generation: preliminary.gate_generation,
                    blocker_codes: preliminary.blocker_codes,
                });
            }
            let runtime_usage = if needs_runtime_facts {
                runtime_usage()
            } else {
                RuntimeUsage::default()
            };
            let snapshot = readiness_snapshot(
                state,
                &catalog,
                self.inner.source_staleness,
                request,
                runtime_usage,
            );
            if snapshot.gate_generation != expected_gate_generation {
                return Ok(BeginMaintenanceResult {
                    status: ReadinessStatus::StaleReadiness,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: vec!["READINESS_GENERATION_STALE".to_string()],
                });
            }
            if snapshot.status != ReadinessStatus::Ready {
                return Ok(BeginMaintenanceResult {
                    status: snapshot.status,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: snapshot.blocker_codes,
                });
            }
            if request.requires_restart && !user_confirmed_restart {
                return Ok(BeginMaintenanceResult {
                    status: ReadinessStatus::UserConfirmationRequired,
                    maintenance_token: None,
                    gate_generation: snapshot.gate_generation,
                    blocker_codes: vec!["USER_CONFIRMATION_REQUIRED".to_string()],
                });
            }
            let record = MaintenanceRecord {
                request_id: request_id.to_string(),
                token: Uuid::new_v4().to_string(),
                target_kind: request.target_kind,
                requires_restart: request.requires_restart,
                user_confirmed_restart,
                expected_gate_generation: snapshot.gate_generation,
                expected_catalog_generation: request.expected_catalog_generation,
                expected_activity_sources: request
                    .expected_activity_sources
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                plan: plan.clone(),
                started_at_unix_ms: now_unix_ms(),
                origin: MaintenanceOrigin::Standard,
            };
            let token = record.token.clone();
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::MaintenanceBegan { record },
            )?;
            Ok(BeginMaintenanceResult {
                status: ReadinessStatus::MaintenanceActive,
                maintenance_token: Some(token),
                gate_generation: state.gate_generation,
                blocker_codes: Vec::new(),
            })
        })
    }

    /// Persists the one permitted pre-Kernel hold for a strictly fresh install.
    ///
    /// Unlike ordinary maintenance, this operation deliberately does not claim
    /// readiness: it only closes admission while the installer proves legacy
    /// processes and external compute resources are absent.
    pub fn begin_core_bootstrap(
        &self,
        request_id: &str,
        plan: &MaintenancePlan,
        request: &ReadinessRequest,
        expected_gate_generation: u64,
        user_confirmed_restart: bool,
    ) -> Result<BeginMaintenanceResult, MaintenanceError> {
        validate_identifier(request_id, "request_id")?;
        validate_plan(plan)?;
        if request.target_kind != UpdateTargetKind::CoreRuntime
            || !request.requires_restart
            || !user_confirmed_restart
        {
            return Err(MaintenanceError::InvalidRequest(
                "core bootstrap requires CORE_RUNTIME restart confirmation".into(),
            ));
        }
        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            let expected_sources = request
                .expected_activity_sources
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if expected_sources.len() != request.expected_activity_sources.len() {
                return Err(MaintenanceError::InvalidRequest(
                    "expected_activity_sources must not contain duplicates".into(),
                ));
            }

            if let Some(existing) = &state.maintenance {
                if existing.request_id == request_id
                    && existing.origin == MaintenanceOrigin::CoreBootstrap
                {
                    let current_sources = catalog.source_ids().into_iter().collect::<Vec<_>>();
                    if &existing.plan != plan
                        || existing.target_kind != request.target_kind
                        || existing.requires_restart != request.requires_restart
                        || !existing.user_confirmed_restart
                        || existing.expected_gate_generation != expected_gate_generation
                        || existing.expected_catalog_generation
                            != request.expected_catalog_generation
                        || existing.expected_catalog_generation != catalog.generation
                        || existing.expected_activity_sources != expected_sources
                        || existing.expected_activity_sources != current_sources
                    {
                        return Err(MaintenanceError::AdmissionDenied(
                            "REQUEST_ID_PLAN_MISMATCH".to_string(),
                        ));
                    }
                    return Ok(BeginMaintenanceResult {
                        status: ReadinessStatus::MaintenanceActive,
                        maintenance_token: Some(existing.token.clone()),
                        gate_generation: state.gate_generation,
                        blocker_codes: vec!["MAINTENANCE_ALREADY_BEGUN".to_string()],
                    });
                }
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_ALREADY_ACTIVE".to_string(),
                ));
            }
            if state.completed_maintenances.contains_key(request_id) {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_REQUEST_ALREADY_COMPLETED".to_string(),
                ));
            }
            if !state.core_bootstrap_eligible
                || !state.completed_maintenances.is_empty()
                || !state.tasks.is_empty()
                || !state.runtime_admissions.is_empty()
                || !state.binding_operations.is_empty()
                || !state.completed_binding_operations.is_empty()
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "CORE_BOOTSTRAP_REQUIRES_FRESH_STORE".to_string(),
                ));
            }
            if state.install_catalog_generation != catalog.generation
                || request.expected_catalog_generation != catalog.generation
                || expected_sources != catalog.source_ids().into_iter().collect::<Vec<_>>()
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "CORE_BOOTSTRAP_CATALOG_MISMATCH".to_string(),
                ));
            }
            if state.gate_generation != expected_gate_generation {
                return Err(MaintenanceError::AdmissionDenied(
                    "CORE_BOOTSTRAP_GATE_GENERATION_MISMATCH".to_string(),
                ));
            }

            let record = MaintenanceRecord {
                request_id: request_id.to_string(),
                token: Uuid::new_v4().to_string(),
                target_kind: UpdateTargetKind::CoreRuntime,
                requires_restart: true,
                user_confirmed_restart: true,
                expected_gate_generation: state.gate_generation,
                expected_catalog_generation: catalog.generation,
                expected_activity_sources: expected_sources,
                plan: plan.clone(),
                started_at_unix_ms: now_unix_ms(),
                origin: MaintenanceOrigin::CoreBootstrap,
            };
            let token = record.token.clone();
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::MaintenanceBegan { record },
            )?;
            Ok(BeginMaintenanceResult {
                status: ReadinessStatus::MaintenanceActive,
                maintenance_token: Some(token),
                gate_generation: state.gate_generation,
                blocker_codes: Vec::new(),
            })
        })
    }

    /// Ends maintenance only after a verified success or healthy rollback.
    pub fn end_maintenance(
        &self,
        request_id: &str,
        token: &str,
        outcome: MaintenanceOutcome,
        healthy: bool,
    ) -> Result<EndMaintenanceResult, MaintenanceError> {
        if token.is_empty() {
            return Err(MaintenanceError::InvalidRequest(
                "maintenance token is required".to_string(),
            ));
        }
        validate_identifier(request_id, "request_id")?;
        self.with_state(|state| {
            if let Some(completed) = state.completed_maintenances.get(request_id) {
                if completed.maintenance.token == token
                    && completed.outcome == outcome
                    && completed.healthy == healthy
                {
                    return Ok(EndMaintenanceResult {
                        unlocked: completed.unlocked,
                        status: if completed.unlocked {
                            ReadinessStatus::Ready
                        } else {
                            ReadinessStatus::MaintenanceActive
                        },
                        gate_generation: completed.gate_generation,
                    });
                }
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_END_REPLAY_MISMATCH".to_string(),
                ));
            }
            let Some(active) = state.maintenance.as_ref() else {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_NOT_ACTIVE".to_string(),
                ));
            };
            if active.token != token || active.request_id != request_id {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_TOKEN_INVALID".to_string(),
                ));
            }
            let unlocked = healthy
                && matches!(
                    outcome,
                    MaintenanceOutcome::Success | MaintenanceOutcome::RolledBack
                );
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::MaintenanceEnded {
                    request_id: request_id.to_string(),
                    token: token.to_string(),
                    outcome,
                    healthy,
                    unlocked,
                    ended_at_unix_ms: now_unix_ms(),
                },
            )?;
            Ok(EndMaintenanceResult {
                unlocked,
                status: if unlocked {
                    ReadinessStatus::Ready
                } else {
                    ReadinessStatus::MaintenanceActive
                },
                gate_generation: state.gate_generation,
            })
        })
    }

    /// Read-only proof that the exact maintenance hold and expected generations are current.
    pub fn validate_maintenance_hold(
        &self,
        proof: &MaintenanceHoldProof,
        target_kind: UpdateTargetKind,
        component_id: &str,
        artifact_digest: &str,
        expected_gate_generation: u64,
        expected_catalog_generation: u64,
    ) -> Result<MaintenanceHoldValidation, MaintenanceError> {
        validate_identifier(&proof.request_id, "request_id")?;
        validate_identifier(component_id, "component_id")?;
        validate_plan(&proof.plan)?;
        if proof.maintenance_token.is_empty() || proof.maintenance_token.len() > 256 {
            return Err(MaintenanceError::InvalidRequest(
                "maintenance token must be non-empty and at most 256 bytes".to_string(),
            ));
        }
        if artifact_digest.is_empty() || artifact_digest.len() > 512 {
            return Err(MaintenanceError::InvalidRequest(
                "artifact digest must be non-empty and at most 512 bytes".to_string(),
            ));
        }

        self.with_state(|state| {
            let catalog = self.catalog_snapshot()?;
            if state.gate_generation != expected_gate_generation {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_GATE_GENERATION_MISMATCH".to_string(),
                ));
            }
            if state.install_catalog_generation != expected_catalog_generation
                || catalog.generation != expected_catalog_generation
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "ACTIVITY_CATALOG_GENERATION_MISMATCH".to_string(),
                ));
            }
            let active = state.maintenance.as_ref().ok_or_else(|| {
                MaintenanceError::AdmissionDenied("MAINTENANCE_NOT_ACTIVE".to_string())
            })?;
            if active.request_id != proof.request_id || active.token != proof.maintenance_token {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_TOKEN_INVALID".to_string(),
                ));
            }
            if active.target_kind != target_kind || active.plan != proof.plan {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_PLAN_MISMATCH".to_string(),
                ));
            }
            if active
                .plan
                .component_artifact_digests
                .get(component_id)
                .is_none_or(|expected| expected != artifact_digest)
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_COMPONENT_DIGEST_MISMATCH".to_string(),
                ));
            }
            if !state.tasks.is_empty()
                || !state.runtime_admissions.is_empty()
                || !state.binding_operations.is_empty()
            {
                return Err(MaintenanceError::AdmissionDenied(
                    "MAINTENANCE_WORK_INFLIGHT".to_string(),
                ));
            }

            Ok(MaintenanceHoldValidation {
                valid: true,
                request_id: active.request_id.clone(),
                target_kind: active.target_kind,
                plan: active.plan.clone(),
                component_id: component_id.to_string(),
                artifact_digest: artifact_digest.to_string(),
                gate_generation: state.gate_generation,
                catalog_generation: state.install_catalog_generation,
            })
        })
    }

    /// Serializes a new Kernel lease or Worker action with BeginMaintenance.
    /// Existing release/stop actions stay available while a gate is active.
    pub fn with_runtime_admission<T>(
        &self,
        action: impl FnOnce() -> Result<T, MaintenanceError>,
    ) -> Result<T, MaintenanceError> {
        match self.with_runtime_admission_named("kernel-action", action) {
            Ok(value) => Ok(value),
            Err(RuntimeAdmissionError::Gate(error) | RuntimeAdmissionError::Action(error)) => {
                Err(error)
            }
        }
    }

    /// Serializes a new Kernel action while preserving its native error type.
    pub fn with_runtime_admission_result<T, E>(
        &self,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, RuntimeAdmissionError<E>> {
        self.with_runtime_admission_named("kernel-action", action)
    }

    /// Reserves a short durable admission marker, releases the file lock, and
    /// then runs the caller action. BeginMaintenance observes this marker and
    /// refuses immediately instead of waiting for a long Worker start.
    pub fn with_runtime_admission_named<T, E>(
        &self,
        action_name: &str,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, RuntimeAdmissionError<E>> {
        validate_identifier(action_name, "action_name").map_err(RuntimeAdmissionError::Gate)?;
        let token = Uuid::new_v4().to_string();
        let reserve = self.with_state(|state| {
            if state.maintenance.is_some() {
                return Err(MaintenanceError::AdmissionDenied(
                    "UPDATE_MAINTENANCE_ACTIVE".to_string(),
                ));
            }
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::RuntimeAdmissionStarted {
                    token: token.clone(),
                    action: action_name.to_string(),
                },
            )
        });
        reserve.map_err(RuntimeAdmissionError::Gate)?;

        let result = action().map_err(RuntimeAdmissionError::Action);
        let finish = self.with_state(|state| {
            if !state.runtime_admissions.contains_key(&token) {
                return Ok(());
            }
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::RuntimeAdmissionEnded {
                    token: token.clone(),
                },
            )
        });
        match (result, finish) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(RuntimeAdmissionError::Gate(error)),
        }
    }

    /// Clears only abandoned Kernel admission markers after runtime recovery succeeded.
    pub fn clear_recovered_runtime_admissions(&self) -> Result<(), MaintenanceError> {
        self.with_state(|state| {
            if state.runtime_admissions.is_empty() {
                return Ok(());
            }
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::RuntimeAdmissionsRecovered,
            )
        })
    }

    fn require_trusted_source(&self, source_id: &str) -> Result<(), MaintenanceError> {
        validate_identifier(source_id, "source_id")?;
        self.refresh_catalog()?;
        self.require_trusted_source_current(source_id)
    }

    fn require_trusted_source_current(&self, source_id: &str) -> Result<(), MaintenanceError> {
        let catalog = self.catalog_snapshot()?;
        if catalog.source(source_id).is_some() {
            Ok(())
        } else {
            Err(MaintenanceError::AdmissionDenied(
                "ACTIVITY_SOURCE_UNTRUSTED".to_string(),
            ))
        }
    }

    fn with_state<T>(
        &self,
        action: impl FnOnce(&mut PersistedState) -> Result<T, MaintenanceError>,
    ) -> Result<T, MaintenanceError> {
        let lock = open_lock_file(&self.inner.directory)?;
        lock.lock()?;
        let mut state = load_state_locked(&self.inner.directory)?;
        let result = self
            .refresh_catalog_locked(&mut state)
            .and_then(|()| action(&mut state));
        let unlock_result = lock.unlock();
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(MaintenanceError::Storage(error)),
        }
    }

    fn catalog_snapshot(&self) -> Result<TrustedActivitySourceCatalog, MaintenanceError> {
        self.inner
            .catalog
            .read()
            .map(|catalog| catalog.clone())
            .map_err(|_| MaintenanceError::StateUnknown("trusted catalog lock poisoned".into()))
    }

    fn refresh_catalog_locked(&self, state: &mut PersistedState) -> Result<(), MaintenanceError> {
        let Some(path) = &self.inner.catalog_path else {
            let catalog = self.catalog_snapshot()?;
            if state.install_catalog_generation != catalog.generation {
                if state.install_catalog_generation > catalog.generation {
                    return Err(MaintenanceError::CatalogUnavailable(
                        "trusted activity-source catalog generation regressed".to_string(),
                    ));
                }
                ensure_catalog_adoption_allowed(state)?;
                append_and_apply(
                    &self.inner.directory,
                    state,
                    JournalEvent::CatalogConfigured {
                        generation: catalog.generation,
                    },
                )?;
            }
            return Ok(());
        };
        let catalog = match TrustedActivitySourceCatalog::load(path) {
            Ok(catalog) => catalog,
            Err(error) => {
                // First-boot tolerance: an absent catalog file on a broker that
                // has never provisioned one (state and in-memory generation
                // both zero) stays unprovisioned and fail-closed instead of
                // failing the whole operation. Only a true ENOENT qualifies;
                // dangling symlinks and other unclassifiable states, any other
                // load failure, and an absent file after a catalog was
                // provisioned still fail closed.
                let never_provisioned = self.catalog_snapshot()?.generation == 0
                    && state.install_catalog_generation == 0
                    && matches!(
                        fs::symlink_metadata(path),
                        Err(ref missing) if missing.kind() == std::io::ErrorKind::NotFound
                    );
                if never_provisioned {
                    return Ok(());
                }
                return Err(error);
            }
        };
        let mut current =
            self.inner.catalog.write().map_err(|_| {
                MaintenanceError::StateUnknown("trusted catalog lock poisoned".into())
            })?;
        if catalog.generation < current.generation
            || (catalog.generation == current.generation && catalog != *current)
        {
            return Err(MaintenanceError::CatalogUnavailable(
                "trusted activity-source catalog changed without a generation advance".to_string(),
            ));
        }
        if state.install_catalog_generation > catalog.generation {
            return Err(MaintenanceError::CatalogUnavailable(
                "trusted activity-source catalog generation regressed".to_string(),
            ));
        }
        if state.install_catalog_generation != catalog.generation {
            ensure_catalog_adoption_allowed(state)?;
        }
        *current = catalog.clone();
        drop(current);
        if state.install_catalog_generation != catalog.generation {
            append_and_apply(
                &self.inner.directory,
                state,
                JournalEvent::CatalogConfigured {
                    generation: catalog.generation,
                },
            )?;
        }
        Ok(())
    }
}

/// Blocks adoption of an out-of-band catalog generation while durable work
/// still depends on the current authority snapshot.
///
/// The supported catalog writer validates the active hold proof or pending
/// operation under this same lock before it updates the file and journal.
/// 中文：避免未受门禁保护的文件替换切断当前操作的授权或维护事务。
fn ensure_catalog_adoption_allowed(state: &PersistedState) -> Result<(), MaintenanceError> {
    if !state.binding_operations.is_empty() {
        return Err(MaintenanceError::AdmissionDenied(
            "BINDING_OPERATIONS_INFLIGHT".to_string(),
        ));
    }
    if state.maintenance.is_some() {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_HOLD_PROOF_REQUIRED".to_string(),
        ));
    }
    Ok(())
}

/// Result wrapper retaining either gate rejection or the caller's action error.
#[derive(Debug)]
pub enum RuntimeAdmissionError<E> {
    Gate(MaintenanceError),
    Action(E),
}

impl<E> RuntimeAdmissionError<E> {
    /// Converts a storage/admission error while leaving caller errors intact.
    pub fn map_action_error<F>(self, map: impl FnOnce(E) -> F) -> RuntimeAdmissionError<F> {
        match self {
            Self::Gate(error) => RuntimeAdmissionError::Gate(error),
            Self::Action(error) => RuntimeAdmissionError::Action(map(error)),
        }
    }
}

fn readiness_snapshot(
    state: &PersistedState,
    catalog: &TrustedActivitySourceCatalog,
    source_staleness: Duration,
    request: &ReadinessRequest,
    runtime_usage: RuntimeUsage,
) -> ReadinessSnapshot {
    let mut active_tasks = active_tasks_for(state, None);
    active_tasks.extend(state.runtime_admissions.iter().map(|(token, action)| {
        TaskActivityRecord {
            source_id: "cyrene.kernel".to_string(),
            task_id: format!("{action}-{token}"),
            state: TaskActivityState::Inflight,
        }
    }));
    let expected_sources = request
        .expected_activity_sources
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let trusted_sources = catalog.source_ids();
    let mut unknown_activity_sources = trusted_sources
        .symmetric_difference(&expected_sources)
        .cloned()
        .collect::<Vec<_>>();
    let now = now_unix_ms();
    for source_id in &trusted_sources {
        if state.sources.get(source_id).is_none_or(|source| {
            now.saturating_sub(source.last_heartbeat_unix_ms) > source_staleness.as_millis() as u64
        }) && !unknown_activity_sources.contains(source_id)
        {
            unknown_activity_sources.push(source_id.clone());
        }
    }
    unknown_activity_sources.sort();
    unknown_activity_sources.dedup();

    let mut blocker_codes = Vec::new();
    let active_binding_operations = state
        .binding_operations
        .values()
        .map(BindingOperationRecord::activity)
        .collect::<Vec<_>>();
    let status = if state.install_catalog_generation != request.expected_catalog_generation
        || state.install_catalog_generation != catalog.generation
        || !unknown_activity_sources.is_empty()
    {
        blocker_codes.push("ACTIVITY_SOURCE_UNKNOWN".to_string());
        ReadinessStatus::Unknown
    } else if state.maintenance.is_some() {
        blocker_codes.push("MAINTENANCE_ALREADY_ACTIVE".to_string());
        ReadinessStatus::MaintenanceActive
    } else if !runtime_usage.known && request.target_kind == UpdateTargetKind::CoreRuntime {
        blocker_codes.push("RUNTIME_ACTIVITY_UNKNOWN".to_string());
        ReadinessStatus::Unknown
    } else if !active_tasks.is_empty() {
        blocker_codes.push("ACTIVE_TASKS_PRESENT".to_string());
        if !active_binding_operations.is_empty() {
            blocker_codes.push("BINDING_OPERATIONS_INFLIGHT".to_string());
        }
        ReadinessStatus::ActiveTasks
    } else if !active_binding_operations.is_empty() {
        blocker_codes.push("BINDING_OPERATIONS_INFLIGHT".to_string());
        ReadinessStatus::ActiveBindingOperations
    } else if request.target_kind == UpdateTargetKind::CoreRuntime
        && (runtime_usage.active_worker_count > 0 || runtime_usage.active_allocation_count > 0)
    {
        blocker_codes.push("IDLE_RUNTIME_REQUIRES_UNLOAD".to_string());
        ReadinessStatus::IdleRuntimeRequiresUnload
    } else {
        ReadinessStatus::Ready
    };
    if request.target_kind == UpdateTargetKind::CoreRuntime
        && state
            .maintenance
            .as_ref()
            .is_some_and(|record| record.origin == MaintenanceOrigin::CoreBootstrap)
        && !runtime_usage.known
    {
        blocker_codes.push("RUNTIME_ACTIVITY_UNKNOWN".to_string());
    }
    ReadinessSnapshot {
        status,
        gate_generation: state.gate_generation,
        install_catalog_generation: catalog.generation,
        active_task_count: active_tasks.len() as u64,
        active_tasks,
        inflight_runtime_admission_count: state.runtime_admissions.len() as u64,
        active_binding_operation_count: active_binding_operations.len() as u64,
        active_binding_operations,
        unknown_activity_sources,
        active_worker_count: runtime_usage.active_worker_count,
        active_allocation_count: runtime_usage.active_allocation_count,
        blocker_codes,
        requires_restart_confirmation: request.requires_restart,
    }
}

fn active_tasks_for(state: &PersistedState, source_id: Option<&str>) -> Vec<TaskActivityRecord> {
    state
        .tasks
        .values()
        .filter(|record| source_id.is_none_or(|source| record.admission.source_id == source))
        .map(|record| TaskActivityRecord {
            source_id: record.admission.source_id.clone(),
            task_id: record.admission.task_id.clone(),
            state: record.admission.state,
        })
        .collect()
}

fn require_fresh_source(
    state: &PersistedState,
    source_id: &str,
    staleness: Duration,
) -> Result<(), MaintenanceError> {
    let Some(source) = state.sources.get(source_id) else {
        return Err(MaintenanceError::AdmissionDenied(
            "ACTIVITY_SOURCE_NOT_READY".to_string(),
        ));
    };
    if now_unix_ms().saturating_sub(source.last_heartbeat_unix_ms) > staleness.as_millis() as u64 {
        return Err(MaintenanceError::AdmissionDenied(
            "ACTIVITY_SOURCE_STALE".to_string(),
        ));
    }
    Ok(())
}

fn ensure_secret_file(path: &Path) -> Result<(), MaintenanceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_private_directory(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file(&mut options);
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(Uuid::new_v4().to_string().as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_private_secret_path(path)?;
            let token = fs::read_to_string(path)?;
            if token.trim().is_empty() {
                return Err(MaintenanceError::StateUnknown(
                    "operator capability file is empty".to_string(),
                ));
            }
            Ok(())
        }
        Err(error) => Err(MaintenanceError::Storage(error)),
    }
}

fn ensure_private_secret_path(path: &Path) -> Result<(), MaintenanceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(
            "secret path must be a regular file".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(MaintenanceError::StateUnknown(
                "secret file permissions must deny group and other access".to_string(),
            ));
        }
        if metadata.uid() != 0 {
            return Err(MaintenanceError::StateUnknown(
                "operator token file must be root-owned".to_string(),
            ));
        }
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn append_and_apply(
    directory: &Path,
    state: &mut PersistedState,
    event: JournalEvent,
) -> Result<(), MaintenanceError> {
    let sequence = state
        .journal_sequence
        .checked_add(1)
        .ok_or_else(|| MaintenanceError::StateUnknown("journal sequence exhausted".into()))?;
    let entry = JournalEntry { sequence, event };
    let journal_path = directory.join(JOURNAL_FILE);
    let mut create_options = OpenOptions::new();
    create_options.write(true).append(true).create_new(true);
    set_shared_file(&mut create_options);
    let mut journal = match create_options.open(&journal_path) {
        Ok(file) => {
            set_file_mode(&file, 0o660)?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut append_options = OpenOptions::new();
            append_options.write(true).append(true);
            append_options.open(&journal_path)?
        }
        Err(error) => return Err(MaintenanceError::Storage(error)),
    };
    serde_json::to_writer(&mut journal, &entry)?;
    journal.write_all(b"\n")?;
    journal.sync_all()?;
    apply_entry(state, &entry)?;
    write_snapshot_atomic(directory, state)?;
    if journal.metadata()?.len() >= JOURNAL_COMPACTION_THRESHOLD {
        write_compacted_journal(directory, state)?;
    }
    Ok(())
}

fn apply_entry(state: &mut PersistedState, entry: &JournalEntry) -> Result<(), MaintenanceError> {
    if entry.sequence != state.journal_sequence.saturating_add(1) {
        return Err(MaintenanceError::StateUnknown(format!(
            "journal sequence {} does not follow {}",
            entry.sequence, state.journal_sequence
        )));
    }
    if !matches!(&entry.event, JournalEvent::CatalogConfigured { .. }) {
        state.core_bootstrap_eligible = false;
    }
    match &entry.event {
        JournalEvent::CatalogConfigured { generation } => {
            state.install_catalog_generation = *generation;
        }
        JournalEvent::SourceHeartbeat {
            source_id,
            at_unix_ms,
        } => {
            record_monotonic_source_heartbeat(state, source_id, *at_unix_ms);
        }
        JournalEvent::TaskAdmitted { admission } | JournalEvent::TaskUpdated { admission } => {
            state.tasks.insert(
                task_key(&admission.source_id, &admission.task_id),
                TaskRecord {
                    admission: admission.clone(),
                },
            );
            state.gate_generation = admission.gate_generation;
        }
        JournalEvent::TaskCompleted { source_id, task_id } => {
            state.tasks.remove(&task_key(source_id, task_id));
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::TasksReconciled {
            source_id,
            active_tasks,
            at_unix_ms,
        } => {
            state
                .tasks
                .retain(|_, value| value.admission.source_id != *source_id);
            for active in active_tasks {
                state.tasks.insert(
                    task_key(source_id, &active.task_id),
                    TaskRecord {
                        admission: active.clone(),
                    },
                );
            }
            record_monotonic_source_heartbeat(state, source_id, *at_unix_ms);
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::StateCheckpoint { .. } => {
            return Err(MaintenanceError::StateUnknown(
                "checkpoint event appeared after journal start".to_string(),
            ));
        }
        JournalEvent::RuntimeAdmissionStarted { token, action } => {
            state
                .runtime_admissions
                .insert(token.clone(), action.clone());
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::RuntimeAdmissionEnded { token } => {
            state.runtime_admissions.remove(token);
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::RuntimeAdmissionsRecovered => {
            state.runtime_admissions.clear();
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::BindingOperationAdmitted { record } => {
            if state.binding_operations.contains_key(&record.request_id)
                || state
                    .completed_binding_operations
                    .contains_key(&record.request_id)
            {
                return Err(MaintenanceError::StateUnknown(
                    "binding operation request id was admitted more than once".to_string(),
                ));
            }
            state
                .binding_operations
                .insert(record.request_id.clone(), record.clone());
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::BindingOperationCompleted { record } => {
            let Some(active) = state.binding_operations.get(&record.request_id) else {
                return Err(MaintenanceError::StateUnknown(
                    "binding operation completion has no active reservation".to_string(),
                ));
            };
            if active != record {
                return Err(MaintenanceError::StateUnknown(
                    "binding operation completion does not match its reservation".to_string(),
                ));
            }
            state.binding_operations.remove(&record.request_id);
            state
                .completed_binding_operations
                .insert(record.request_id.clone(), record.clone());
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::MaintenanceBegan { record } => {
            state.maintenance = Some(record.clone());
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
        JournalEvent::MaintenanceEnded {
            request_id,
            token,
            outcome,
            healthy,
            unlocked,
            ended_at_unix_ms,
        } => {
            let Some(active) = state.maintenance.as_ref() else {
                return Err(MaintenanceError::StateUnknown(
                    "maintenance end journal record has no active transaction".to_string(),
                ));
            };
            if active.request_id != *request_id || active.token != *token {
                return Err(MaintenanceError::StateUnknown(
                    "maintenance end journal record does not match the active transaction"
                        .to_string(),
                ));
            }
            if *unlocked
                != (*healthy
                    && matches!(
                        outcome,
                        MaintenanceOutcome::Success | MaintenanceOutcome::RolledBack
                    ))
            {
                return Err(MaintenanceError::StateUnknown(
                    "maintenance end journal health and unlock outcome disagree".to_string(),
                ));
            }
            if *unlocked {
                let maintenance = active.clone();
                state.maintenance = None;
                state.completed_maintenances.insert(
                    request_id.clone(),
                    CompletedMaintenanceRecord {
                        maintenance,
                        outcome: *outcome,
                        healthy: *healthy,
                        unlocked: true,
                        ended_at_unix_ms: *ended_at_unix_ms,
                        gate_generation: state.gate_generation.saturating_add(1),
                    },
                );
            }
            state.gate_generation = state.gate_generation.saturating_add(1);
        }
    }
    state.journal_sequence = entry.sequence;
    Ok(())
}

/// Retains monotonic durable heartbeat timestamps if the wall clock steps back.
fn record_monotonic_source_heartbeat(
    state: &mut PersistedState,
    source_id: &str,
    observed_at_unix_ms: u64,
) {
    state
        .sources
        .entry(source_id.to_string())
        .and_modify(|source| {
            source.last_heartbeat_unix_ms = source.last_heartbeat_unix_ms.max(observed_at_unix_ms);
        })
        .or_insert(SourceRecord {
            last_heartbeat_unix_ms: observed_at_unix_ms,
        });
}

/// Accepts only the exact persisted top-level fields for a known state profile.
fn validate_persisted_state_shape(
    value: &serde_json::Value,
    schema_version: u32,
    legacy_profile: Option<LegacyStateProfile>,
) -> Result<(), MaintenanceError> {
    let object = value.as_object().ok_or_else(|| {
        MaintenanceError::StateUnknown("persisted state must be a JSON object".to_string())
    })?;
    let common = [
        "schema_version",
        "journal_sequence",
        "gate_generation",
        "install_catalog_generation",
        "maintenance",
        "completed_maintenances",
        "tasks",
        "runtime_admissions",
        "sources",
    ];
    let has_active_binding = object.contains_key("binding_operations");
    let has_completed_binding = object.contains_key("completed_binding_operations");

    if schema_version == LEGACY_STATE_SCHEMA_VERSION {
        let profile = legacy_profile.ok_or_else(|| {
            MaintenanceError::StateUnknown(
                "schema-1 state requires an explicit migration profile".to_string(),
            )
        })?;
        let layout_matches = match profile {
            LegacyStateProfile::ReleasedV1NoBindingAdmissions => {
                !has_active_binding && !has_completed_binding
            }
            LegacyStateProfile::ExperimentalV1BindingAdmissions => {
                has_active_binding && has_completed_binding
            }
        };
        if !layout_matches {
            return Err(MaintenanceError::StateUnknown(
                "schema-1 binding-operation fields do not match the declared migration profile"
                    .to_string(),
            ));
        }
    } else if schema_version == STATE_SCHEMA_VERSION {
        if !has_active_binding
            || !has_completed_binding
            || !object.contains_key("core_bootstrap_eligible")
        {
            return Err(MaintenanceError::StateUnknown(
                "schema-2 state is missing required persisted fields".to_string(),
            ));
        }
    } else {
        return Err(MaintenanceError::StateUnknown(
            "unsupported maintenance state schema".to_string(),
        ));
    }

    let mut allowed = common.into_iter().collect::<BTreeSet<_>>();
    if schema_version == STATE_SCHEMA_VERSION
        || matches!(
            legacy_profile,
            Some(LegacyStateProfile::ExperimentalV1BindingAdmissions)
        )
    {
        allowed.insert("binding_operations");
        allowed.insert("completed_binding_operations");
    }
    if schema_version == STATE_SCHEMA_VERSION || object.contains_key("core_bootstrap_eligible") {
        allowed.insert("core_bootstrap_eligible");
    }
    if object.keys().any(|key| !allowed.contains(key.as_str())) {
        return Err(MaintenanceError::StateUnknown(
            "persisted state contains an unknown field".to_string(),
        ));
    }
    if common.iter().any(|key| !object.contains_key(*key)) {
        return Err(MaintenanceError::StateUnknown(
            "persisted state is missing a required field".to_string(),
        ));
    }
    Ok(())
}

/// Checks checkpoint shape before serde defaults could conceal a lost map.
fn validate_journal_entry_shape(
    line: &[u8],
    schema_version: u32,
    legacy_profile: Option<LegacyStateProfile>,
) -> Result<(), MaintenanceError> {
    let value: serde_json::Value = serde_json::from_slice(line).map_err(|error| {
        MaintenanceError::StateUnknown(format!("journal entry parse failed: {error}"))
    })?;
    let object = value.as_object().ok_or_else(|| {
        MaintenanceError::StateUnknown("journal entry must be a JSON object".to_string())
    })?;
    if object.len() != 2 || !object.contains_key("sequence") || !object.contains_key("event") {
        return Err(MaintenanceError::StateUnknown(
            "journal entry contains missing or unknown fields".to_string(),
        ));
    }
    let event = object
        .get("event")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            MaintenanceError::StateUnknown("journal event must be a JSON object".to_string())
        })?;
    let kind = event
        .get("event")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MaintenanceError::StateUnknown("journal event kind is missing".to_string())
        })?;
    if legacy_profile == Some(LegacyStateProfile::ReleasedV1NoBindingAdmissions)
        && matches!(
            kind,
            "binding_operation_admitted" | "binding_operation_completed"
        )
    {
        return Err(MaintenanceError::StateUnknown(
            "released schema-1 profile contains binding-operation events".to_string(),
        ));
    }
    if kind == "state_checkpoint" {
        let checkpoint = event.get("state").ok_or_else(|| {
            MaintenanceError::StateUnknown("journal checkpoint state is missing".to_string())
        })?;
        validate_persisted_state_shape(checkpoint, schema_version, legacy_profile)?;
    }
    Ok(())
}

fn load_state_locked(directory: &Path) -> Result<PersistedState, MaintenanceError> {
    load_state_locked_with_schema(directory, STATE_SCHEMA_VERSION, None, true)
}

/// Reads one known persistence generation. Migration uses `repair=false` so an
/// incomplete source is never rewritten before its exact layout is verified.
fn load_state_locked_with_schema(
    directory: &Path,
    expected_schema_version: u32,
    legacy_profile: Option<LegacyStateProfile>,
    repair: bool,
) -> Result<PersistedState, MaintenanceError> {
    let state_path = directory.join(STATE_FILE);
    let journal_path = directory.join(JOURNAL_FILE);
    let state_existed = state_path.exists();
    let mut state = match fs::read(&state_path) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                MaintenanceError::StateUnknown(format!("snapshot parse failed: {error}"))
            })?;
            validate_persisted_state_shape(&value, expected_schema_version, legacy_profile)?;
            serde_json::from_value::<PersistedState>(value).map_err(|error| {
                MaintenanceError::StateUnknown(format!("snapshot parse failed: {error}"))
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
        Err(error) => return Err(MaintenanceError::Storage(error)),
    };
    if state.schema_version != expected_schema_version {
        return Err(MaintenanceError::StateUnknown(
            "unsupported maintenance snapshot schema".to_string(),
        ));
    }
    let snapshot_sequence = state.journal_sequence;
    let journal_bytes = match fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !state_existed => {
            return Ok(state);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MaintenanceError::StateUnknown(
                "journal missing for an existing maintenance snapshot".to_string(),
            ));
        }
        Err(error) => return Err(MaintenanceError::Storage(error)),
    };
    if journal_bytes.is_empty() {
        if state.journal_sequence != 0 {
            return Err(MaintenanceError::StateUnknown(
                "maintenance journal is empty for a nonempty snapshot".to_string(),
            ));
        }
        return Ok(state);
    }

    let mut last_sequence = None;
    let mut valid_bytes = 0_usize;
    let mut needs_terminal_newline = false;
    let chunks = journal_bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    for (index, chunk) in chunks.iter().enumerate() {
        let terminated = chunk.last() == Some(&b'\n');
        let line = if terminated {
            &chunk[..chunk.len() - 1]
        } else {
            *chunk
        };
        let entry = match serde_json::from_slice::<JournalEntry>(line) {
            Ok(entry) => entry,
            Err(error) if index + 1 == chunks.len() && !terminated => {
                if state.journal_sequence > last_sequence.unwrap_or_default() {
                    return Err(MaintenanceError::StateUnknown(format!(
                        "snapshot is newer than the valid maintenance journal tail: {error}"
                    )));
                }
                if repair {
                    let file = OpenOptions::new().write(true).open(&journal_path)?;
                    file.set_len(valid_bytes as u64)?;
                    file.sync_all()?;
                } else {
                    return Err(MaintenanceError::StateUnknown(
                        "maintenance journal has an incomplete final record".to_string(),
                    ));
                }
                break;
            }
            Err(error) => {
                return Err(MaintenanceError::StateUnknown(format!(
                    "malformed maintenance journal record: {error}"
                )));
            }
        };

        validate_journal_entry_shape(line, expected_schema_version, legacy_profile)?;

        if let Some(last_sequence) = last_sequence {
            let expected = last_sequence.saturating_add(1);
            if entry.sequence != expected {
                return Err(MaintenanceError::StateUnknown(
                    "maintenance journal sequence is discontinuous".to_string(),
                ));
            }
            if entry.sequence > state.journal_sequence {
                apply_entry(&mut state, &entry)?;
            }
        } else {
            match &entry.event {
                JournalEvent::StateCheckpoint { state: checkpoint } => {
                    if entry.sequence == 0
                        || checkpoint.schema_version != expected_schema_version
                        || checkpoint.journal_sequence != entry.sequence
                    {
                        return Err(MaintenanceError::StateUnknown(
                            "maintenance journal checkpoint is inconsistent".to_string(),
                        ));
                    }
                    if checkpoint.journal_sequence > state.journal_sequence {
                        state = (**checkpoint).clone();
                    }
                }
                _ if entry.sequence != 1 => {
                    return Err(MaintenanceError::StateUnknown(
                        "maintenance journal does not start at sequence one or a checkpoint"
                            .to_string(),
                    ));
                }
                _ if entry.sequence > state.journal_sequence => apply_entry(&mut state, &entry)?,
                _ => {}
            }
        }
        last_sequence = Some(entry.sequence);
        valid_bytes += chunk.len();
        needs_terminal_newline = !terminated;
    }

    let Some(last_sequence) = last_sequence else {
        if state.journal_sequence == 0 {
            return Ok(state);
        }
        return Err(MaintenanceError::StateUnknown(
            "maintenance journal contains no valid records".to_string(),
        ));
    };
    if needs_terminal_newline {
        if repair {
            let mut file = OpenOptions::new().append(true).open(&journal_path)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        } else {
            return Err(MaintenanceError::StateUnknown(
                "maintenance journal is missing its terminal newline".to_string(),
            ));
        }
    }
    if state.journal_sequence > last_sequence {
        return Err(MaintenanceError::StateUnknown(
            "maintenance journal is truncated before the durable snapshot".to_string(),
        ));
    }
    if repair && state.journal_sequence > snapshot_sequence {
        write_snapshot_atomic(directory, &state)?;
    }
    Ok(state)
}

/// Converts one proven schema-1 layout to schema 2 without allowing an ordinary
/// broker or Kernel open to guess which legacy fields were present.
pub fn migrate_state_schema1(
    directory: impl AsRef<Path>,
    proof: &StateMigrationProof,
) -> Result<StateMigrationResult, MaintenanceError> {
    require_root_for_schema_change()?;
    let directory = directory.as_ref();
    validate_migration_proof(proof)?;
    prepare_offline_state_directory(directory)?;
    let lock = open_existing_lock_file(directory)?;
    lock.lock()?;
    let result = migrate_state_schema1_locked(directory, proof);
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(MaintenanceError::Storage(error)),
    }
}

/// Restores a prior schema-1 writer only while a matching hold remains active
/// and schema-2 changes are limited to compatible startup metadata.
pub fn rollback_state_schema1(
    directory: impl AsRef<Path>,
    proof: &StateMigrationProof,
) -> Result<StateMigrationResult, MaintenanceError> {
    require_root_for_schema_change()?;
    let directory = directory.as_ref();
    validate_migration_proof(proof)?;
    prepare_offline_state_directory(directory)?;
    let lock = open_existing_lock_file(directory)?;
    lock.lock()?;
    let result = rollback_state_schema1_locked(directory, proof);
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(MaintenanceError::Storage(error)),
    }
}

fn migrate_state_schema1_locked(
    directory: &Path,
    proof: &StateMigrationProof,
) -> Result<StateMigrationResult, MaintenanceError> {
    let marker_path = directory.join(STATE_MIGRATION_MARKER_FILE);
    match fs::symlink_metadata(&marker_path) {
        Ok(_) => {
            let mut marker = read_migration_marker(directory)?;
            validate_marker(&marker, directory)?;
            ensure_marker_matches_proof(&marker, proof)?;
            verify_migration_backups(directory, &marker)?;
            match marker.phase {
                StateMigrationPhase::Prepared
                | StateMigrationPhase::JournalReplaced
                | StateMigrationPhase::SnapshotReplaced => {
                    resume_schema1_migration(directory, &mut marker)?;
                }
                StateMigrationPhase::Complete => {
                    let live = load_state_locked_with_schema(
                        directory,
                        STATE_SCHEMA_VERSION,
                        None,
                        false,
                    )?;
                    validate_held_state_for_rollback(&live, &marker)?;
                    return Ok(StateMigrationResult {
                        migrated: true,
                        schema_version: STATE_SCHEMA_VERSION,
                        migration_id: marker.migration_id,
                        gate_generation: live.gate_generation,
                        catalog_generation: live.install_catalog_generation,
                    });
                }
                StateMigrationPhase::RollbackPrepared
                | StateMigrationPhase::RollbackJournalReplaced
                | StateMigrationPhase::RollbackSnapshotReplaced => {
                    return Err(MaintenanceError::StateUnknown(
                        "state rollback is in progress; use rollback-state to resume".to_string(),
                    ));
                }
            }
            return migration_result(&marker);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(MaintenanceError::Storage(error)),
    }

    let source_state_path = directory.join(STATE_FILE);
    let source_journal_path = directory.join(JOURNAL_FILE);
    let source_state = read_regular_bytes(&source_state_path)?;
    let source_journal = read_regular_bytes(&source_journal_path)?;
    let legacy = load_state_locked_with_schema(
        directory,
        LEGACY_STATE_SCHEMA_VERSION,
        Some(proof.schema1_profile),
        false,
    )?;
    validate_held_state(&legacy, proof, true)?;

    let mut migrated = legacy.clone();
    migrated.schema_version = STATE_SCHEMA_VERSION;
    let target_state = serde_json::to_vec(&migrated)?;
    validate_persisted_state_shape(
        &serde_json::from_slice(&target_state).map_err(|error| {
            MaintenanceError::StateUnknown(format!("migrated state serialization failed: {error}"))
        })?,
        STATE_SCHEMA_VERSION,
        None,
    )?;
    let target_journal = compacted_journal_bytes(&migrated)?;

    let migration_id = Uuid::new_v4().to_string();
    let mut marker = StateMigrationMarker {
        migration_version: 1,
        migration_id: migration_id.clone(),
        from_schema_version: LEGACY_STATE_SCHEMA_VERSION,
        to_schema_version: STATE_SCHEMA_VERSION,
        phase: StateMigrationPhase::Prepared,
        schema1_profile: proof.schema1_profile,
        request_id: proof.request_id.clone(),
        maintenance_token_sha256: digest(proof.maintenance_token.as_bytes()),
        target_kind: proof.target_kind,
        plan_id: proof.plan_id.clone(),
        plan_digest: proof.plan_digest.clone(),
        component_artifact_digests: proof.component_artifact_digests.clone(),
        expected_gate_generation: proof.expected_gate_generation,
        expected_catalog_generation: proof.expected_catalog_generation,
        source_state_sha256: digest(&source_state),
        source_journal_sha256: digest(&source_journal),
        target_state_sha256: digest(&target_state),
        target_journal_sha256: digest(&target_journal),
        backup_state_file: format!("maintenance-state.schema1-backup-{migration_id}.json"),
        backup_journal_file: format!("maintenance-journal.schema1-backup-{migration_id}.jsonl"),
        backup_schema2_state_file: format!("maintenance-state.schema2-backup-{migration_id}.json"),
        backup_schema2_journal_file: format!(
            "maintenance-journal.schema2-backup-{migration_id}.jsonl"
        ),
        rollback_from_state_sha256: None,
        rollback_from_journal_sha256: None,
        rollback_target_state_sha256: None,
        rollback_target_journal_sha256: None,
        rollback_target_state_file: None,
        rollback_target_journal_file: None,
    };

    write_private_backup(&directory.join(&marker.backup_state_file), &source_state)?;
    write_private_backup(
        &directory.join(&marker.backup_journal_file),
        &source_journal,
    )?;
    write_private_backup(&marker.backup_schema2_state_path(directory), &target_state)?;
    write_private_backup(
        &marker.backup_schema2_journal_path(directory),
        &target_journal,
    )?;
    verify_migration_backups(directory, &marker)?;
    write_migration_marker(directory, &marker)?;
    resume_schema1_migration(directory, &mut marker)?;
    migration_result(&marker)
}

fn resume_schema1_migration(
    directory: &Path,
    marker: &mut StateMigrationMarker,
) -> Result<(), MaintenanceError> {
    let state_path = directory.join(STATE_FILE);
    let journal_path = directory.join(JOURNAL_FILE);
    let state_hash = digest(&read_regular_bytes(&state_path)?);
    let journal_hash = digest(&read_regular_bytes(&journal_path)?);
    let source_pair =
        state_hash == marker.source_state_sha256 && journal_hash == marker.source_journal_sha256;
    let journal_replaced_pair =
        state_hash == marker.source_state_sha256 && journal_hash == marker.target_journal_sha256;
    let target_pair =
        state_hash == marker.target_state_sha256 && journal_hash == marker.target_journal_sha256;
    if !(source_pair || journal_replaced_pair || target_pair) {
        return Err(MaintenanceError::StateUnknown(
            "migration state files do not match a known source/target phase".to_string(),
        ));
    }
    if marker.phase == StateMigrationPhase::Complete {
        if !target_pair {
            return Err(MaintenanceError::StateUnknown(
                "completed migration files no longer match the target hashes".to_string(),
            ));
        }
        return Ok(());
    }
    if matches!(
        marker.phase,
        StateMigrationPhase::RollbackPrepared
            | StateMigrationPhase::RollbackJournalReplaced
            | StateMigrationPhase::RollbackSnapshotReplaced
    ) {
        return Err(MaintenanceError::StateUnknown(
            "rollback marker cannot be resumed as a migration".to_string(),
        ));
    }

    let target_state = read_regular_bytes(&marker.backup_schema2_state_path(directory))?;
    let target_journal = read_regular_bytes(&marker.backup_schema2_journal_path(directory))?;
    if digest(&target_state) != marker.target_state_sha256
        || digest(&target_journal) != marker.target_journal_sha256
    {
        return Err(MaintenanceError::StateUnknown(
            "schema-2 migration target backup does not match the marker".to_string(),
        ));
    }
    if source_pair {
        atomic_replace_bytes(&journal_path, &target_journal, 0o660)?;
    }
    if marker.phase == StateMigrationPhase::Prepared {
        marker.phase = StateMigrationPhase::JournalReplaced;
        write_migration_marker(directory, marker)?;
    }
    let current_journal_hash = digest(&read_regular_bytes(&journal_path)?);
    let current_state_hash = digest(&read_regular_bytes(&state_path)?);
    if current_journal_hash != marker.target_journal_sha256
        || !matches!(current_state_hash.as_str(), hash if hash == marker.source_state_sha256 || hash == marker.target_state_sha256)
    {
        return Err(MaintenanceError::StateUnknown(
            "migration resume found an unknown state/journal pair".to_string(),
        ));
    }
    if current_state_hash == marker.source_state_sha256 {
        atomic_replace_bytes(&state_path, &target_state, 0o660)?;
    }
    marker.phase = StateMigrationPhase::SnapshotReplaced;
    write_migration_marker(directory, marker)?;
    let migrated = load_state_locked_with_schema(directory, STATE_SCHEMA_VERSION, None, false)?;
    if migrated.gate_generation != marker.expected_gate_generation
        || migrated.install_catalog_generation != marker.expected_catalog_generation
        || !held_state_matches_marker(&migrated, marker)
    {
        return Err(MaintenanceError::StateUnknown(
            "migrated state no longer matches the active maintenance hold".to_string(),
        ));
    }
    marker.phase = StateMigrationPhase::Complete;
    write_migration_marker(directory, marker)?;
    Ok(())
}

fn rollback_state_schema1_locked(
    directory: &Path,
    proof: &StateMigrationProof,
) -> Result<StateMigrationResult, MaintenanceError> {
    let mut marker = read_migration_marker(directory)?;
    validate_marker(&marker, directory)?;
    ensure_marker_matches_proof(&marker, proof)?;
    verify_migration_backups(directory, &marker)?;

    if !matches!(
        marker.phase,
        StateMigrationPhase::RollbackPrepared
            | StateMigrationPhase::RollbackJournalReplaced
            | StateMigrationPhase::RollbackSnapshotReplaced
    ) {
        if marker.phase == StateMigrationPhase::Complete {
            let baseline_bytes = read_private_backup(&marker.backup_schema2_state_path(directory))?;
            let baseline: PersistedState =
                serde_json::from_slice(&baseline_bytes).map_err(|error| {
                    MaintenanceError::StateUnknown(format!(
                        "schema-2 rollback baseline is invalid: {error}"
                    ))
                })?;
            let current =
                load_state_locked_with_schema(directory, STATE_SCHEMA_VERSION, None, false)?;
            validate_held_state_for_rollback(&current, &marker)?;
            let safe_gate_advances = validate_rollback_journal_delta(
                directory,
                baseline.journal_sequence,
                baseline.install_catalog_generation,
                current.journal_sequence,
                current.install_catalog_generation,
            )?;
            ensure_rollback_compatible_delta(&current, &baseline, safe_gate_advances)?;
            let (rollback_state, rollback_journal) =
                rollback_schema1_bytes(&current, marker.schema1_profile)?;
            let migration_id = &marker.migration_id;
            let rollback_state_name =
                format!("maintenance-state.rollback-target-{migration_id}.json");
            let rollback_journal_name =
                format!("maintenance-journal.rollback-target-{migration_id}.jsonl");
            write_private_backup_idempotent(
                &directory.join(&rollback_state_name),
                &rollback_state,
            )?;
            write_private_backup_idempotent(
                &directory.join(&rollback_journal_name),
                &rollback_journal,
            )?;
            marker.rollback_from_state_sha256 =
                Some(digest(&read_regular_bytes(&directory.join(STATE_FILE))?));
            marker.rollback_from_journal_sha256 =
                Some(digest(&read_regular_bytes(&directory.join(JOURNAL_FILE))?));
            marker.rollback_target_state_sha256 = Some(digest(&rollback_state));
            marker.rollback_target_journal_sha256 = Some(digest(&rollback_journal));
            marker.rollback_target_state_file = Some(rollback_state_name);
            marker.rollback_target_journal_file = Some(rollback_journal_name);
            marker.phase = StateMigrationPhase::RollbackPrepared;
            write_migration_marker(directory, &marker)?;
        } else if matches!(
            marker.phase,
            StateMigrationPhase::Prepared
                | StateMigrationPhase::JournalReplaced
                | StateMigrationPhase::SnapshotReplaced
        ) {
            // Before the new cohort has opened schema 2, restore the exact v1
            // backup pair. The in-progress marker kept every writer closed.
            let current_state_hash = digest(&read_regular_bytes(&directory.join(STATE_FILE))?);
            let current_journal_hash = digest(&read_regular_bytes(&directory.join(JOURNAL_FILE))?);
            let pair_is_known = if current_state_hash == marker.source_state_sha256 {
                current_journal_hash == marker.source_journal_sha256
                    || current_journal_hash == marker.target_journal_sha256
            } else if current_state_hash == marker.target_state_sha256 {
                current_journal_hash == marker.target_journal_sha256
            } else {
                false
            };
            if !pair_is_known {
                return Err(MaintenanceError::StateUnknown(
                    "in-progress migration files do not match a known rollback phase".to_string(),
                ));
            }
            let source_state = read_private_backup(&marker.backup_state_path(directory))?;
            let source_journal = read_private_backup(&marker.backup_journal_path(directory))?;
            restore_legacy_pair(directory, &mut marker, &source_state, &source_journal)?;
            return migration_result(&marker);
        }
    }

    resume_schema1_rollback(directory, &mut marker)?;
    let restored = load_state_locked_with_schema(
        directory,
        LEGACY_STATE_SCHEMA_VERSION,
        Some(marker.schema1_profile),
        false,
    )?;
    let mut result = migration_result(&marker)?;
    result.gate_generation = restored.gate_generation;
    result.catalog_generation = restored.install_catalog_generation;
    Ok(result)
}

fn resume_schema1_rollback(
    directory: &Path,
    marker: &mut StateMigrationMarker,
) -> Result<(), MaintenanceError> {
    let (from_state, from_journal, target_state, target_journal, state_name, journal_name) =
        rollback_artifacts(marker)?;
    let state_path = directory.join(STATE_FILE);
    let journal_path = directory.join(JOURNAL_FILE);
    let state_hash = digest(&read_regular_bytes(&state_path)?);
    let journal_hash = digest(&read_regular_bytes(&journal_path)?);
    let before = state_hash == from_state && journal_hash == from_journal;
    let journal_replaced = state_hash == from_state && journal_hash == target_journal;
    let target_pair = state_hash == target_state && journal_hash == target_journal;
    if !(before || journal_replaced || target_pair) {
        return Err(MaintenanceError::StateUnknown(
            "rollback files do not match a known source/target phase".to_string(),
        ));
    }
    let target_state_bytes = read_private_backup(&directory.join(state_name))?;
    let target_journal_bytes = read_private_backup(&directory.join(journal_name))?;
    if digest(&target_state_bytes) != target_state
        || digest(&target_journal_bytes) != target_journal
    {
        return Err(MaintenanceError::StateUnknown(
            "rollback target backup does not match the marker".to_string(),
        ));
    }
    if before {
        atomic_replace_bytes(&journal_path, &target_journal_bytes, 0o660)?;
    }
    if marker.phase == StateMigrationPhase::RollbackPrepared {
        marker.phase = StateMigrationPhase::RollbackJournalReplaced;
        write_migration_marker(directory, marker)?;
    }
    if digest(&read_regular_bytes(&state_path)?) == from_state {
        atomic_replace_bytes(&state_path, &target_state_bytes, 0o660)?;
    }
    marker.phase = StateMigrationPhase::RollbackSnapshotReplaced;
    write_migration_marker(directory, marker)?;
    let restored = load_state_locked_with_schema(
        directory,
        LEGACY_STATE_SCHEMA_VERSION,
        Some(marker.schema1_profile),
        false,
    )?;
    if !held_state_identity_matches_marker(&restored, marker)
        || restored.gate_generation < marker.expected_gate_generation
        || restored.install_catalog_generation < marker.expected_catalog_generation
    {
        return Err(MaintenanceError::StateUnknown(
            "restored schema-1 state does not preserve the active maintenance hold".to_string(),
        ));
    }
    remove_migration_marker(directory)?;
    Ok(())
}

fn restore_legacy_pair(
    directory: &Path,
    marker: &mut StateMigrationMarker,
    source_state: &[u8],
    source_journal: &[u8],
) -> Result<(), MaintenanceError> {
    if digest(source_state) != marker.source_state_sha256
        || digest(source_journal) != marker.source_journal_sha256
    {
        return Err(MaintenanceError::StateUnknown(
            "schema-1 backup pair does not match its source hashes".to_string(),
        ));
    }
    atomic_replace_bytes(&directory.join(JOURNAL_FILE), source_journal, 0o660)?;
    atomic_replace_bytes(&directory.join(STATE_FILE), source_state, 0o660)?;
    let restored = load_state_locked_with_schema(
        directory,
        LEGACY_STATE_SCHEMA_VERSION,
        Some(marker.schema1_profile),
        false,
    )?;
    if !held_state_matches_marker(&restored, marker) {
        return Err(MaintenanceError::StateUnknown(
            "schema-1 backup did not retain the active maintenance hold".to_string(),
        ));
    }
    marker.phase = StateMigrationPhase::RollbackSnapshotReplaced;
    remove_migration_marker(directory)
}

fn validate_migration_proof(proof: &StateMigrationProof) -> Result<(), MaintenanceError> {
    if proof.target_kind != UpdateTargetKind::CoreRuntime {
        return Err(MaintenanceError::InvalidRequest(
            "state migration requires a CORE_RUNTIME maintenance hold".to_string(),
        ));
    }
    validate_identifier(&proof.request_id, "request_id")?;
    if proof.maintenance_token.is_empty() || proof.maintenance_token.len() > 256 {
        return Err(MaintenanceError::InvalidRequest(
            "maintenance token must be non-empty and at most 256 bytes".to_string(),
        ));
    }
    validate_plan(&MaintenancePlan {
        plan_id: proof.plan_id.clone(),
        plan_digest: proof.plan_digest.clone(),
        component_artifact_digests: proof.component_artifact_digests.clone(),
    })?;
    for component in ["cyrene-kernel", "cyrene-runtime-maintenance"] {
        if !proof.component_artifact_digests.contains_key(component) {
            return Err(MaintenanceError::InvalidRequest(format!(
                "migration proof is missing required component digest: {component}"
            )));
        }
    }
    if proof.expected_gate_generation == 0 || proof.expected_catalog_generation == 0 {
        return Err(MaintenanceError::InvalidRequest(
            "migration proof generations must be nonzero".to_string(),
        ));
    }
    Ok(())
}

fn require_root_for_schema_change() -> Result<(), MaintenanceError> {
    if nix::unistd::geteuid().as_raw() != 0 && !cfg!(test) {
        return Err(MaintenanceError::AdmissionDenied(
            "ROOT_OPERATOR_REQUIRED".to_string(),
        ));
    }
    Ok(())
}

fn validate_held_state(
    state: &PersistedState,
    proof: &StateMigrationProof,
    require_original_catalog_generation: bool,
) -> Result<(), MaintenanceError> {
    if state.schema_version != LEGACY_STATE_SCHEMA_VERSION
        || state.gate_generation != proof.expected_gate_generation
        || (require_original_catalog_generation
            && state.install_catalog_generation != proof.expected_catalog_generation)
        || state.install_catalog_generation < proof.expected_catalog_generation
    {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_GENERATION_MISMATCH".to_string(),
        ));
    }
    let active = state
        .maintenance
        .as_ref()
        .ok_or_else(|| MaintenanceError::AdmissionDenied("MAINTENANCE_NOT_ACTIVE".to_string()))?;
    let plan = MaintenancePlan {
        plan_id: proof.plan_id.clone(),
        plan_digest: proof.plan_digest.clone(),
        component_artifact_digests: proof.component_artifact_digests.clone(),
    };
    if active.request_id != proof.request_id || active.token != proof.maintenance_token {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_TOKEN_INVALID".to_string(),
        ));
    }
    if active.target_kind != UpdateTargetKind::CoreRuntime
        || !active.requires_restart
        || !active.user_confirmed_restart
        || active.expected_catalog_generation != proof.expected_catalog_generation
        || active.plan != plan
    {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_PLAN_MISMATCH".to_string(),
        ));
    }
    ensure_migration_has_no_runtime_work(state)
}

fn ensure_migration_has_no_runtime_work(state: &PersistedState) -> Result<(), MaintenanceError> {
    if !state.tasks.is_empty()
        || !state.runtime_admissions.is_empty()
        || !state.binding_operations.is_empty()
    {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_WORK_INFLIGHT".to_string(),
        ));
    }
    Ok(())
}

fn held_state_matches_marker(state: &PersistedState, marker: &StateMigrationMarker) -> bool {
    state.gate_generation == marker.expected_gate_generation
        && held_state_identity_matches_marker(state, marker)
}

fn held_state_identity_matches_marker(
    state: &PersistedState,
    marker: &StateMigrationMarker,
) -> bool {
    state.install_catalog_generation >= marker.expected_catalog_generation
        && state.maintenance.as_ref().is_some_and(|active| {
            active.request_id == marker.request_id
                && digest(active.token.as_bytes()) == marker.maintenance_token_sha256
                && active.target_kind == UpdateTargetKind::CoreRuntime
                && active.requires_restart
                && active.user_confirmed_restart
                && active.expected_catalog_generation == marker.expected_catalog_generation
                && active.plan.plan_id == marker.plan_id
                && active.plan.plan_digest == marker.plan_digest
                && active.plan.component_artifact_digests == marker.component_artifact_digests
        })
        && state.tasks.is_empty()
        && state.runtime_admissions.is_empty()
        && state.binding_operations.is_empty()
}

fn validate_held_state_for_rollback(
    state: &PersistedState,
    marker: &StateMigrationMarker,
) -> Result<(), MaintenanceError> {
    if state.schema_version != STATE_SCHEMA_VERSION
        || !held_state_identity_matches_marker(state, marker)
        || state.gate_generation < marker.expected_gate_generation
    {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_HOLD_PROOF_MISMATCH".to_string(),
        ));
    }
    Ok(())
}

fn ensure_rollback_compatible_delta(
    current: &PersistedState,
    baseline: &PersistedState,
    allowed_gate_advances: u64,
) -> Result<(), MaintenanceError> {
    if current.schema_version != STATE_SCHEMA_VERSION
        || baseline.schema_version != STATE_SCHEMA_VERSION
        || current.journal_sequence < baseline.journal_sequence
        || current.gate_generation
            != baseline
                .gate_generation
                .saturating_add(allowed_gate_advances)
        || current.install_catalog_generation < baseline.install_catalog_generation
        || (!baseline.core_bootstrap_eligible && current.core_bootstrap_eligible)
    {
        return Err(MaintenanceError::StateUnknown(
            "schema-2 changes are not safe for rollback".to_string(),
        ));
    }
    for (source_id, previous) in &baseline.sources {
        let Some(current_source) = current.sources.get(source_id) else {
            return Err(MaintenanceError::StateUnknown(
                "rollback would remove a persisted activity source".to_string(),
            ));
        };
        if current_source.last_heartbeat_unix_ms < previous.last_heartbeat_unix_ms {
            return Err(MaintenanceError::StateUnknown(
                "rollback would regress an activity-source heartbeat".to_string(),
            ));
        }
    }

    let mut current_value = serde_json::to_value(current)?;
    let baseline_value = serde_json::to_value(baseline)?;
    let current_object = current_value.as_object_mut().ok_or_else(|| {
        MaintenanceError::StateUnknown("current state did not serialize as an object".to_string())
    })?;
    let baseline_object = baseline_value.as_object().ok_or_else(|| {
        MaintenanceError::StateUnknown("baseline did not serialize as an object".to_string())
    })?;
    for field in [
        "journal_sequence",
        "gate_generation",
        "install_catalog_generation",
        "sources",
        "core_bootstrap_eligible",
    ] {
        current_object.insert(
            field.to_string(),
            baseline_object.get(field).cloned().ok_or_else(|| {
                MaintenanceError::StateUnknown(format!("baseline is missing {field}"))
            })?,
        );
    }
    if current_value != baseline_value {
        return Err(MaintenanceError::StateUnknown(
            "rollback found a task, lease, binding, hold, or schema-2-only state change"
                .to_string(),
        ));
    }
    Ok(())
}

/// Whitelists only journal writes with no task, lease, or binding side effects.
fn validate_rollback_journal_delta(
    directory: &Path,
    baseline_sequence: u64,
    baseline_catalog_generation: u64,
    current_sequence: u64,
    current_catalog_generation: u64,
) -> Result<u64, MaintenanceError> {
    let bytes = read_regular_bytes(&directory.join(JOURNAL_FILE))?;
    if !bytes.ends_with(b"\n") {
        return Err(MaintenanceError::StateUnknown(
            "rollback journal has an incomplete final record".to_string(),
        ));
    }
    let chunks = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let mut previous_sequence: Option<u64> = None;
    let mut catalog_generation = baseline_catalog_generation;
    let mut safe_gate_advances = 0_u64;
    for (index, line) in chunks.iter().enumerate() {
        if line.is_empty() {
            if index + 1 == chunks.len() {
                continue;
            }
            return Err(MaintenanceError::StateUnknown(
                "rollback journal contains an empty record".to_string(),
            ));
        }
        let entry: JournalEntry = serde_json::from_slice(line).map_err(|error| {
            MaintenanceError::StateUnknown(format!("rollback journal entry is invalid: {error}"))
        })?;
        validate_journal_entry_shape(line, STATE_SCHEMA_VERSION, None)?;
        if let Some(previous) = previous_sequence {
            if entry.sequence != previous.saturating_add(1) {
                return Err(MaintenanceError::StateUnknown(
                    "rollback journal sequence is discontinuous".to_string(),
                ));
            }
        } else {
            match &entry.event {
                JournalEvent::StateCheckpoint { state }
                    if entry.sequence == baseline_sequence
                        && state.journal_sequence == baseline_sequence => {}
                _ => {
                    return Err(MaintenanceError::StateUnknown(
                        "rollback journal no longer contains the migration baseline checkpoint"
                            .to_string(),
                    ));
                }
            }
        }
        previous_sequence = Some(entry.sequence);
        if entry.sequence <= baseline_sequence {
            continue;
        }
        match &entry.event {
            JournalEvent::SourceHeartbeat { .. } => {}
            JournalEvent::CatalogConfigured { generation } => {
                if *generation != catalog_generation.saturating_add(1) {
                    return Err(MaintenanceError::StateUnknown(
                        "rollback catalog generation changes are not sequential".to_string(),
                    ));
                }
                catalog_generation = *generation;
            }
            JournalEvent::TasksReconciled { active_tasks, .. } if active_tasks.is_empty() => {
                safe_gate_advances = safe_gate_advances.saturating_add(1);
            }
            _ => {
                return Err(MaintenanceError::StateUnknown(
                    "rollback journal contains a task, runtime admission, binding, or hold mutation"
                        .to_string(),
                ));
            }
        }
    }
    if previous_sequence != Some(current_sequence)
        || catalog_generation != current_catalog_generation
    {
        return Err(MaintenanceError::StateUnknown(
            "rollback journal does not account for the current state generations".to_string(),
        ));
    }
    Ok(safe_gate_advances)
}

fn rollback_schema1_bytes(
    current: &PersistedState,
    profile: LegacyStateProfile,
) -> Result<(Vec<u8>, Vec<u8>), MaintenanceError> {
    let mut legacy = current.clone();
    legacy.schema_version = LEGACY_STATE_SCHEMA_VERSION;
    let mut state_value = serde_json::to_value(&legacy)?;
    let object = state_value.as_object_mut().ok_or_else(|| {
        MaintenanceError::StateUnknown("rollback state did not serialize as an object".to_string())
    })?;
    object.insert(
        "schema_version".to_string(),
        serde_json::Value::from(LEGACY_STATE_SCHEMA_VERSION),
    );
    if profile == LegacyStateProfile::ReleasedV1NoBindingAdmissions {
        if !legacy.binding_operations.is_empty() || !legacy.completed_binding_operations.is_empty()
        {
            return Err(MaintenanceError::StateUnknown(
                "released schema-1 profile cannot represent binding-operation records".to_string(),
            ));
        }
        object.remove("binding_operations");
        object.remove("completed_binding_operations");
    }
    validate_persisted_state_shape(&state_value, LEGACY_STATE_SCHEMA_VERSION, Some(profile))?;
    let state_bytes = serde_json::to_vec(&state_value)?;
    let entry = serde_json::json!({
        "sequence": legacy.journal_sequence,
        "event": {
            "event": "state_checkpoint",
            "state": state_value,
        }
    });
    let mut journal = serde_json::to_vec(&entry)?;
    journal.push(b'\n');
    Ok((state_bytes, journal))
}

fn compacted_journal_bytes(state: &PersistedState) -> Result<Vec<u8>, MaintenanceError> {
    let entry = JournalEntry {
        sequence: state.journal_sequence,
        event: JournalEvent::StateCheckpoint {
            state: Box::new(state.clone()),
        },
    };
    let mut bytes = serde_json::to_vec(&entry)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn migration_result(
    marker: &StateMigrationMarker,
) -> Result<StateMigrationResult, MaintenanceError> {
    let state_version = if matches!(
        marker.phase,
        StateMigrationPhase::RollbackPrepared
            | StateMigrationPhase::RollbackJournalReplaced
            | StateMigrationPhase::RollbackSnapshotReplaced
    ) {
        LEGACY_STATE_SCHEMA_VERSION
    } else {
        STATE_SCHEMA_VERSION
    };
    Ok(StateMigrationResult {
        migrated: state_version == STATE_SCHEMA_VERSION,
        schema_version: state_version,
        migration_id: marker.migration_id.clone(),
        gate_generation: marker.expected_gate_generation,
        catalog_generation: marker.expected_catalog_generation,
    })
}

fn rollback_artifacts(
    marker: &StateMigrationMarker,
) -> Result<(String, String, String, String, String, String), MaintenanceError> {
    let required = |value: Option<&String>, field: &str| {
        value.cloned().ok_or_else(|| {
            MaintenanceError::StateUnknown(format!("rollback marker is missing {field}"))
        })
    };
    Ok((
        required(
            marker.rollback_from_state_sha256.as_ref(),
            "rollback_from_state_sha256",
        )?,
        required(
            marker.rollback_from_journal_sha256.as_ref(),
            "rollback_from_journal_sha256",
        )?,
        required(
            marker.rollback_target_state_sha256.as_ref(),
            "rollback_target_state_sha256",
        )?,
        required(
            marker.rollback_target_journal_sha256.as_ref(),
            "rollback_target_journal_sha256",
        )?,
        required(
            marker.rollback_target_state_file.as_ref(),
            "rollback_target_state_file",
        )?,
        required(
            marker.rollback_target_journal_file.as_ref(),
            "rollback_target_journal_file",
        )?,
    ))
}

fn prepare_offline_state_directory(directory: &Path) -> Result<(), MaintenanceError> {
    reject_symlink_directory(directory)?;
    validate_shared_files(directory)?;
    for name in [STATE_FILE, JOURNAL_FILE, LOCK_FILE] {
        let metadata = fs::symlink_metadata(directory.join(name))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MaintenanceError::StateUnknown(format!(
                "offline state migration requires an existing regular {name}"
            )));
        }
    }
    Ok(())
}

fn open_existing_lock_file(directory: &Path) -> Result<File, MaintenanceError> {
    let path = directory.join(LOCK_FILE);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(
            "maintenance lock must be an existing regular file".to_string(),
        ));
    }
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

fn read_regular_bytes(path: &Path) -> Result<Vec<u8>, MaintenanceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(format!(
            "{} must be a regular non-symlink file",
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

fn read_private_backup(path: &Path) -> Result<Vec<u8>, MaintenanceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(format!(
            "{} must be a regular rollback backup",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let test_owner = cfg!(test) && metadata.uid() == nix::unistd::geteuid().as_raw();
        if (metadata.uid() != 0 && !test_owner) || metadata.permissions().mode() & 0o077 != 0 {
            return Err(MaintenanceError::StateUnknown(
                "rollback backups must be root-owned mode 0600".to_string(),
            ));
        }
    }
    fs::read(path).map_err(MaintenanceError::Storage)
}

fn write_private_backup(path: &Path, bytes: &[u8]) -> Result<(), MaintenanceError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file(&mut options);
    let mut file = options.open(path)?;
    set_file_mode(&file, 0o600)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()?;
    Ok(())
}

fn write_private_backup_idempotent(path: &Path, bytes: &[u8]) -> Result<(), MaintenanceError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            if read_private_backup(path)? == bytes {
                Ok(())
            } else {
                Err(MaintenanceError::StateUnknown(
                    "existing rollback target does not match regenerated bytes".to_string(),
                ))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_private_backup(path, bytes)
        }
        Err(error) => Err(MaintenanceError::Storage(error)),
    }
}

fn atomic_replace_bytes(path: &Path, bytes: &[u8], mode: u32) -> Result<(), MaintenanceError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(".state-migration-tmp-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_shared_file(&mut options);
    let mut file = options.open(&temp_path)?;
    set_file_mode(&file, mode)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp_path, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_migration_marker(
    directory: &Path,
    marker: &StateMigrationMarker,
) -> Result<(), MaintenanceError> {
    let marker_path = directory.join(STATE_MIGRATION_MARKER_FILE);
    if let Ok(metadata) = fs::symlink_metadata(&marker_path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MaintenanceError::StateUnknown(
                "refusing to replace an unknown migration marker".to_string(),
            ));
        }
    }
    let bytes = serde_json::to_vec_pretty(marker)?;
    atomic_replace_bytes(&marker_path, &bytes, 0o640)
}

fn read_migration_marker(directory: &Path) -> Result<StateMigrationMarker, MaintenanceError> {
    let path = directory.join(STATE_MIGRATION_MARKER_FILE);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(
            "state migration marker must be a regular non-symlink file".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory_gid = fs::metadata(directory)?.gid();
        let test_owner = cfg!(test) && metadata.uid() == nix::unistd::geteuid().as_raw();
        if (metadata.uid() != 0 && !test_owner)
            || metadata.gid() != directory_gid
            || metadata.permissions().mode() & 0o007 != 0
            || metadata.permissions().mode() & 0o040 == 0
            || metadata.permissions().mode() & 0o020 != 0
        {
            return Err(MaintenanceError::StateUnknown(
                "state migration marker must be root-owned and authority-group-readable"
                    .to_string(),
            ));
        }
    }
    let marker = serde_json::from_slice(&fs::read(path)?).map_err(|error| {
        MaintenanceError::StateUnknown(format!("state migration marker is invalid: {error}"))
    })?;
    Ok(marker)
}

fn validate_marker(
    marker: &StateMigrationMarker,
    directory: &Path,
) -> Result<(), MaintenanceError> {
    if marker.migration_version != 1
        || marker.from_schema_version != LEGACY_STATE_SCHEMA_VERSION
        || marker.to_schema_version != STATE_SCHEMA_VERSION
        || marker.expected_gate_generation == 0
        || marker.expected_catalog_generation == 0
        || Uuid::parse_str(&marker.migration_id)
            .map(|id| id.to_string() != marker.migration_id)
            .unwrap_or(true)
        || marker.target_kind != UpdateTargetKind::CoreRuntime
        || marker.backup_state_file
            != format!(
                "maintenance-state.schema1-backup-{}.json",
                marker.migration_id
            )
        || marker.backup_journal_file
            != format!(
                "maintenance-journal.schema1-backup-{}.jsonl",
                marker.migration_id
            )
        || marker.backup_schema2_state_file
            != format!(
                "maintenance-state.schema2-backup-{}.json",
                marker.migration_id
            )
        || marker.backup_schema2_journal_file
            != format!(
                "maintenance-journal.schema2-backup-{}.jsonl",
                marker.migration_id
            )
    {
        return Err(MaintenanceError::StateUnknown(
            "state migration marker identity is unsupported".to_string(),
        ));
    }
    validate_identifier(&marker.request_id, "request_id")?;
    validate_plan(&MaintenancePlan {
        plan_id: marker.plan_id.clone(),
        plan_digest: marker.plan_digest.clone(),
        component_artifact_digests: marker.component_artifact_digests.clone(),
    })?;
    for component in ["cyrene-kernel", "cyrene-runtime-maintenance"] {
        if !marker.component_artifact_digests.contains_key(component) {
            return Err(MaintenanceError::StateUnknown(format!(
                "migration marker is missing component digest {component}"
            )));
        }
    }
    for hash in [
        &marker.source_state_sha256,
        &marker.source_journal_sha256,
        &marker.target_state_sha256,
        &marker.target_journal_sha256,
        &marker.maintenance_token_sha256,
    ] {
        validate_digest(hash, "migration marker hash")?;
    }
    let rollback_phase = matches!(
        marker.phase,
        StateMigrationPhase::RollbackPrepared
            | StateMigrationPhase::RollbackJournalReplaced
            | StateMigrationPhase::RollbackSnapshotReplaced
    );
    let rollback_fields = [
        marker.rollback_from_state_sha256.as_ref(),
        marker.rollback_from_journal_sha256.as_ref(),
        marker.rollback_target_state_sha256.as_ref(),
        marker.rollback_target_journal_sha256.as_ref(),
        marker.rollback_target_state_file.as_ref(),
        marker.rollback_target_journal_file.as_ref(),
    ];
    let all_rollback_fields_present = rollback_fields.iter().all(Option::is_some);
    let all_rollback_fields_absent = rollback_fields.iter().all(Option::is_none);
    if (rollback_phase && !all_rollback_fields_present)
        || (!rollback_phase && !all_rollback_fields_absent)
    {
        return Err(MaintenanceError::StateUnknown(
            "state migration marker rollback fields do not match its phase".to_string(),
        ));
    }
    if rollback_phase {
        for hash in [
            marker.rollback_from_state_sha256.as_ref().unwrap(),
            marker.rollback_from_journal_sha256.as_ref().unwrap(),
            marker.rollback_target_state_sha256.as_ref().unwrap(),
            marker.rollback_target_journal_sha256.as_ref().unwrap(),
        ] {
            validate_digest(hash, "rollback marker hash")?;
        }
        let state_file = format!(
            "maintenance-state.rollback-target-{}.json",
            marker.migration_id
        );
        let journal_file = format!(
            "maintenance-journal.rollback-target-{}.jsonl",
            marker.migration_id
        );
        if marker.rollback_target_state_file.as_deref() != Some(state_file.as_str())
            || marker.rollback_target_journal_file.as_deref() != Some(journal_file.as_str())
        {
            return Err(MaintenanceError::StateUnknown(
                "rollback marker target filenames are unsupported".to_string(),
            ));
        }
    }
    let _ = directory;
    Ok(())
}

fn verify_migration_backups(
    directory: &Path,
    marker: &StateMigrationMarker,
) -> Result<(), MaintenanceError> {
    for (path, expected) in [
        (
            marker.backup_state_path(directory),
            &marker.source_state_sha256,
        ),
        (
            marker.backup_journal_path(directory),
            &marker.source_journal_sha256,
        ),
        (
            marker.backup_schema2_state_path(directory),
            &marker.target_state_sha256,
        ),
        (
            marker.backup_schema2_journal_path(directory),
            &marker.target_journal_sha256,
        ),
    ] {
        if digest(&read_private_backup(&path)?) != *expected {
            return Err(MaintenanceError::StateUnknown(
                "a migration backup hash does not match the marker".to_string(),
            ));
        }
    }
    let state1 = read_private_backup(&marker.backup_state_path(directory))?;
    let state1_value: serde_json::Value = serde_json::from_slice(&state1).map_err(|error| {
        MaintenanceError::StateUnknown(format!("schema-1 backup state is invalid: {error}"))
    })?;
    validate_persisted_state_shape(
        &state1_value,
        LEGACY_STATE_SCHEMA_VERSION,
        Some(marker.schema1_profile),
    )?;
    let state2 = read_private_backup(&marker.backup_schema2_state_path(directory))?;
    let state2_value: serde_json::Value = serde_json::from_slice(&state2).map_err(|error| {
        MaintenanceError::StateUnknown(format!("schema-2 backup state is invalid: {error}"))
    })?;
    validate_persisted_state_shape(&state2_value, STATE_SCHEMA_VERSION, None)?;
    let journal2 = read_private_backup(&marker.backup_schema2_journal_path(directory))?;
    if !journal2.ends_with(b"\n") {
        return Err(MaintenanceError::StateUnknown(
            "schema-2 checkpoint backup has no terminal newline".to_string(),
        ));
    }
    validate_journal_entry_shape(
        &journal2[..journal2.len().saturating_sub(1)],
        STATE_SCHEMA_VERSION,
        None,
    )?;
    Ok(())
}

fn ensure_marker_matches_proof(
    marker: &StateMigrationMarker,
    proof: &StateMigrationProof,
) -> Result<(), MaintenanceError> {
    if marker.schema1_profile != proof.schema1_profile
        || marker.request_id != proof.request_id
        || marker.maintenance_token_sha256 != digest(proof.maintenance_token.as_bytes())
        || marker.target_kind != proof.target_kind
        || marker.plan_id != proof.plan_id
        || marker.plan_digest != proof.plan_digest
        || marker.component_artifact_digests != proof.component_artifact_digests
        || marker.expected_gate_generation != proof.expected_gate_generation
        || marker.expected_catalog_generation != proof.expected_catalog_generation
    {
        return Err(MaintenanceError::AdmissionDenied(
            "MAINTENANCE_HOLD_PROOF_MISMATCH".to_string(),
        ));
    }
    Ok(())
}

fn ensure_migration_marker_allows_state2(directory: &Path) -> Result<(), MaintenanceError> {
    let path = directory.join(STATE_MIGRATION_MARKER_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MaintenanceError::Storage(error)),
        Ok(_) => {
            let marker = read_migration_marker(directory)?;
            validate_marker(&marker, directory)?;
            if marker.phase == StateMigrationPhase::Complete {
                Ok(())
            } else {
                Err(MaintenanceError::StateUnknown(
                    "schema migration or rollback is incomplete".to_string(),
                ))
            }
        }
    }
}

fn remove_migration_marker(directory: &Path) -> Result<(), MaintenanceError> {
    let path = directory.join(STATE_MIGRATION_MARKER_FILE);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::StateUnknown(
            "refusing to remove an unknown migration marker".to_string(),
        ));
    }
    fs::remove_file(path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn write_compacted_journal(
    directory: &Path,
    state: &PersistedState,
) -> Result<(), MaintenanceError> {
    let temp_path = directory.join(format!("{JOURNAL_FILE}.tmp-{}", Uuid::new_v4()));
    let final_path = directory.join(JOURNAL_FILE);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_shared_file(&mut options);
    let mut file = options.open(&temp_path)?;
    set_file_mode(&file, 0o660)?;
    let entry = JournalEntry {
        sequence: state.journal_sequence,
        event: JournalEvent::StateCheckpoint {
            state: Box::new(state.clone()),
        },
    };
    serde_json::to_writer(&mut file, &entry)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp_path, &final_path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn write_snapshot_atomic(directory: &Path, state: &PersistedState) -> Result<(), MaintenanceError> {
    let temp_path = directory.join(format!("{STATE_FILE}.tmp-{}", Uuid::new_v4()));
    let final_path = directory.join(STATE_FILE);
    let bytes = serde_json::to_vec(state)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_shared_file(&mut options);
    let mut file = options.open(&temp_path)?;
    set_file_mode(&file, 0o660)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temp_path, &final_path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn open_lock_file(directory: &Path) -> Result<File, MaintenanceError> {
    let path = directory.join(LOCK_FILE);
    let mut create_options = OpenOptions::new();
    create_options.read(true).write(true).create_new(true);
    set_shared_file(&mut create_options);
    match create_options.open(&path) {
        Ok(file) => {
            set_file_mode(&file, 0o660)?;
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing_options = OpenOptions::new();
            existing_options.read(true).write(true);
            Ok(existing_options.open(path)?)
        }
        Err(error) => Err(MaintenanceError::Storage(error)),
    }
}

fn set_file_mode(file: &File, mode: u32) -> Result<(), MaintenanceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (file, mode);
    Ok(())
}

fn reject_symlink_directory(directory: &Path) -> Result<(), MaintenanceError> {
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MaintenanceError::CatalogUnavailable(
            "maintenance state directory must be a real directory".to_string(),
        ));
    }
    Ok(())
}

fn reject_symlink_file(path: &Path) -> Result<(), MaintenanceError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        MaintenanceError::CatalogUnavailable(format!("{}: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MaintenanceError::CatalogUnavailable(
            "trusted catalog path must be a regular file".to_string(),
        ));
    }
    Ok(())
}

fn validate_catalog_permissions(path: &Path) -> Result<(), MaintenanceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::metadata(path)?;
        if metadata.permissions().mode() & 0o022 != 0 || metadata.uid() != 0 {
            return Err(MaintenanceError::CatalogUnavailable(
                "trusted catalog must be root-owned and not group/other writable".to_string(),
            ));
        }
    }
    Ok(())
}

fn set_private_directory(path: &Path) -> Result<(), MaintenanceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_shared_directory(path: &Path, allow_test_owner: bool) -> Result<(), MaintenanceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::symlink_metadata(path)?;
        let owner = metadata.uid();
        let mode = metadata.permissions().mode() & 0o7777;
        if owner == 0 && nix::unistd::geteuid().as_raw() == 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o2770))?;
        } else if owner == 0 && mode == 0o2770 {
            // Kernel is a member of the dedicated authority group but does not own this path.
        } else if (allow_test_owner || cfg!(test)) && owner == nix::unistd::geteuid().as_raw() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o2770))?;
        } else {
            return Err(MaintenanceError::StateUnknown(
                "shared state directory must be root-owned and mode 2770".to_string(),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = allow_test_owner;
    Ok(())
}

fn validate_shared_files(directory: &Path) -> Result<(), MaintenanceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory_gid = fs::metadata(directory)?.gid();
        for name in [STATE_FILE, JOURNAL_FILE, LOCK_FILE] {
            let path = directory.join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(MaintenanceError::Storage(error)),
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.gid() != directory_gid
                || metadata.permissions().mode() & 0o007 != 0
                || metadata.permissions().mode() & 0o060 != 0o060
            {
                return Err(MaintenanceError::StateUnknown(format!(
                    "shared state file {} must be a regular file in the authority group with mode 0660",
                    path.display()
                )));
            }
        }
        let operator_hash = directory.join(OPERATOR_TOKEN_HASH_FILE);
        if let Ok(metadata) = fs::symlink_metadata(&operator_hash) {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.gid() != directory_gid
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o007 != 0
                || metadata.permissions().mode() & 0o040 == 0
                || metadata.permissions().mode() & 0o020 != 0
            {
                return Err(MaintenanceError::StateUnknown(
                    "operator hash must be a regular authority-group-readable file with mode 0640"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn set_private_file(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
}

fn set_shared_file(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o660);
    }
}

fn write_private_hash_atomic(path: &Path, hash: &[u8]) -> Result<(), MaintenanceError> {
    let temp_path = path.with_extension(format!("sha256.tmp-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o640);
    }
    let mut file = options.open(&temp_path)?;
    set_file_mode(&file, 0o640)?;
    file.write_all(hash)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp_path, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn task_key(source_id: &str, task_id: &str) -> String {
    format!("{source_id}\0{task_id}")
}

fn validate_plan(plan: &MaintenancePlan) -> Result<(), MaintenanceError> {
    validate_identifier(&plan.plan_id, "plan_id")?;
    validate_digest(&plan.plan_digest, "plan_digest")?;
    if plan.component_artifact_digests.is_empty() {
        return Err(MaintenanceError::InvalidRequest(
            "component_artifact_digests must contain at least one artifact".to_string(),
        ));
    }
    for (component_id, digest) in &plan.component_artifact_digests {
        validate_identifier(component_id, "component_id")?;
        validate_digest(digest, "component_artifact_digest")?;
    }
    Ok(())
}

fn validate_binding_operation_scope(scope: &BindingOperationScope) -> Result<(), MaintenanceError> {
    validate_identifier(&scope.binding_id, "binding_id")?;
    validate_identifier(&scope.package_id, "package_id")?;
    validate_identifier(&scope.installation_id, "installation_id")
}

fn same_binding_operation(
    stored: &BindingOperationRecord,
    requested: &BindingOperationRecord,
) -> bool {
    stored.request_id == requested.request_id
        && stored.source_id == requested.source_id
        && stored.scope == requested.scope
}

fn validate_digest(value: &str, field: &str) -> Result<(), MaintenanceError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(MaintenanceError::InvalidRequest(format!(
            "{field} must use sha256:<64 lowercase hexadecimal characters>"
        )))
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), MaintenanceError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(MaintenanceError::InvalidRequest(format!(
            "{field} must be 1-256 ASCII letters, digits, '.', '_', '-', or ':'"
        )));
    }
    Ok(())
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u64::MAX as u128) as u64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        sync::Barrier,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    fn setup() -> (TempDir, RuntimeMaintenance, ReadinessRequest) {
        let dir = TempDir::new().unwrap();
        let catalog = TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: 7,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-catalogs".to_string(),
                uid: 1001,
                gid: Some(1000),
                source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
                binding_scopes: Vec::new(),
            }],
        };
        let maintenance = RuntimeMaintenance::open(dir.path(), catalog).unwrap();
        maintenance
            .heartbeat_activity_source("cyrene-catalogs")
            .unwrap();
        let request = ReadinessRequest {
            target_kind: UpdateTargetKind::PackageOnly,
            requires_restart: false,
            expected_catalog_generation: 7,
            expected_activity_sources: vec!["cyrene-catalogs".to_string()],
        };
        (dir, maintenance, request)
    }

    fn package_usage() -> RuntimeUsage {
        RuntimeUsage {
            known: true,
            active_worker_count: 0,
            active_allocation_count: 0,
        }
    }

    fn package_plan() -> MaintenancePlan {
        MaintenancePlan {
            plan_id: "platform-update-27".to_string(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
            component_artifact_digests: BTreeMap::from([(
                "cyrene-yield".to_string(),
                format!("sha256:{}", "b".repeat(64)),
            )]),
        }
    }

    fn binding_test_catalog() -> TrustedActivitySourceCatalog {
        let make_scope = |binding_id: &str| TrustedBindingScope {
            binding_id: binding_id.to_string(),
            package_id: "llf.trainer".to_string(),
            installation_ids: vec!["llf.install.2026".to_string()],
            operations: vec![
                BindingOperationKind::Activate,
                BindingOperationKind::Recover,
                BindingOperationKind::Deactivate,
            ],
        };
        TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: 12,
            sources: vec![
                TrustedActivitySource {
                    source_id: "cyrene-other".to_string(),
                    uid: 1002,
                    gid: Some(1002),
                    source_token_sha256: format!("{:x}", Sha256::digest(b"other-source-token")),
                    binding_scopes: vec![make_scope("binding-other")],
                },
                TrustedActivitySource {
                    source_id: "cyrene-yield".to_string(),
                    uid: 1001,
                    gid: Some(1000),
                    source_token_sha256: format!("{:x}", Sha256::digest(b"yield-source-token")),
                    binding_scopes: vec![make_scope("binding-main")],
                },
            ],
        }
    }

    #[test]
    fn replayed_source_heartbeats_never_regress_when_wall_clock_moves_back() {
        let mut state = PersistedState::default();
        state.sources.insert(
            "cyrene-kernel".to_string(),
            SourceRecord {
                last_heartbeat_unix_ms: 100,
            },
        );

        apply_entry(
            &mut state,
            &JournalEntry {
                sequence: 1,
                event: JournalEvent::SourceHeartbeat {
                    source_id: "cyrene-kernel".to_string(),
                    at_unix_ms: 99,
                },
            },
        )
        .unwrap();
        assert_eq!(state.sources["cyrene-kernel"].last_heartbeat_unix_ms, 100);

        apply_entry(
            &mut state,
            &JournalEntry {
                sequence: 2,
                event: JournalEvent::TasksReconciled {
                    source_id: "cyrene-kernel".to_string(),
                    active_tasks: Vec::new(),
                    at_unix_ms: 98,
                },
            },
        )
        .unwrap();
        assert_eq!(state.sources["cyrene-kernel"].last_heartbeat_unix_ms, 100);
    }

    fn binding_setup() -> (
        TempDir,
        RuntimeMaintenance,
        ReadinessRequest,
        BindingOperationCaller,
        BindingOperationScope,
        BindingOperationCaller,
        BindingOperationScope,
    ) {
        let dir = TempDir::new().unwrap();
        let catalog = binding_test_catalog();
        let maintenance = RuntimeMaintenance::open(dir.path(), catalog).unwrap();
        maintenance
            .heartbeat_activity_source("cyrene-yield")
            .unwrap();
        maintenance
            .heartbeat_activity_source("cyrene-other")
            .unwrap();
        let request = ReadinessRequest {
            target_kind: UpdateTargetKind::PackageOnly,
            requires_restart: false,
            expected_catalog_generation: 12,
            expected_activity_sources: vec!["cyrene-other".to_string(), "cyrene-yield".to_string()],
        };
        let caller = BindingOperationCaller {
            source_id: "cyrene-yield".to_string(),
            source_token: "yield-source-token".to_string(),
            peer_uid: 1001,
            peer_gid: 1000,
            expected_catalog_generation: 12,
        };
        let scope = BindingOperationScope {
            binding_id: "binding-main".to_string(),
            package_id: "llf.trainer".to_string(),
            installation_id: "llf.install.2026".to_string(),
            operation: BindingOperationKind::Activate,
        };
        let other_caller = BindingOperationCaller {
            source_id: "cyrene-other".to_string(),
            source_token: "other-source-token".to_string(),
            peer_uid: 1002,
            peer_gid: 1002,
            expected_catalog_generation: 12,
        };
        let other_scope = BindingOperationScope {
            binding_id: "binding-other".to_string(),
            package_id: "llf.trainer".to_string(),
            installation_id: "llf.install.2026".to_string(),
            operation: BindingOperationKind::Activate,
        };
        (
            dir,
            maintenance,
            request,
            caller,
            scope,
            other_caller,
            other_scope,
        )
    }

    fn bootstrap_setup() -> (TempDir, RuntimeMaintenance, ReadinessRequest) {
        let dir = TempDir::new().unwrap();
        let catalog = TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: 7,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-catalogs".to_string(),
                uid: 1001,
                gid: Some(1000),
                source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
                binding_scopes: Vec::new(),
            }],
        };
        let maintenance = RuntimeMaintenance::open(dir.path(), catalog).unwrap();
        let request = ReadinessRequest {
            target_kind: UpdateTargetKind::CoreRuntime,
            requires_restart: true,
            expected_catalog_generation: 7,
            expected_activity_sources: vec!["cyrene-catalogs".to_string()],
        };
        (dir, maintenance, request)
    }

    fn legacy_migration_fixture(
        profile: LegacyStateProfile,
    ) -> (TempDir, StateMigrationProof, TrustedActivitySourceCatalog) {
        let dir = TempDir::new().unwrap();
        let catalog = TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: 7,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-kernel".to_string(),
                uid: nix::unistd::getuid().as_raw(),
                gid: Some(nix::unistd::getgid().as_raw()),
                source_token_sha256: format!("{:x}", Sha256::digest(b"kernel-source-token")),
                binding_scopes: Vec::new(),
            }],
        };
        let gate = RuntimeMaintenance::open(dir.path(), catalog.clone()).unwrap();
        gate.heartbeat_activity_source("cyrene-kernel").unwrap();
        let readiness = ReadinessRequest {
            target_kind: UpdateTargetKind::CoreRuntime,
            requires_restart: true,
            expected_catalog_generation: catalog.generation,
            expected_activity_sources: vec!["cyrene-kernel".to_string()],
        };
        let plan = MaintenancePlan {
            plan_id: "c10-core-runtime-plan".to_string(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
            component_artifact_digests: BTreeMap::from([
                (
                    "cyrene-kernel".to_string(),
                    format!("sha256:{}", "b".repeat(64)),
                ),
                (
                    "cyrene-runtime-maintenance".to_string(),
                    format!("sha256:{}", "c".repeat(64)),
                ),
            ]),
        };
        let expected_gate_generation = gate.current_gate_generation().unwrap();
        let begin = gate
            .begin_maintenance(
                "c10-migration-hold",
                &plan,
                &readiness,
                expected_gate_generation,
                true,
                RuntimeUsage {
                    known: true,
                    active_worker_count: 0,
                    active_allocation_count: 0,
                },
            )
            .unwrap();
        assert_eq!(begin.status, ReadinessStatus::MaintenanceActive);
        let maintenance_token = begin.maintenance_token.unwrap();
        drop(gate);

        let mut state = load_state_locked(dir.path()).unwrap();
        if profile == LegacyStateProfile::ExperimentalV1BindingAdmissions {
            state.completed_binding_operations.insert(
                "completed-before-c10".to_string(),
                BindingOperationRecord {
                    request_id: "completed-before-c10".to_string(),
                    operation_token: "operation-receipt-token".to_string(),
                    source_id: "cyrene-yield".to_string(),
                    scope: BindingOperationScope {
                        binding_id: "binding-before-c10".to_string(),
                        package_id: "llf.trainer".to_string(),
                        installation_id: "install-before-c10".to_string(),
                        operation: BindingOperationKind::Activate,
                    },
                    started_at_unix_ms: 1_728_000_000_000,
                },
            );
        }
        state.schema_version = LEGACY_STATE_SCHEMA_VERSION;
        let mut state_value = serde_json::to_value(&state).unwrap();
        if profile == LegacyStateProfile::ReleasedV1NoBindingAdmissions {
            let object = state_value.as_object_mut().unwrap();
            object.remove("binding_operations");
            object.remove("completed_binding_operations");
        }
        fs::write(
            dir.path().join(STATE_FILE),
            serde_json::to_vec(&state_value).unwrap(),
        )
        .unwrap();
        let checkpoint = serde_json::json!({
            "sequence": state.journal_sequence,
            "event": {"event": "state_checkpoint", "state": state_value},
        });
        let mut journal_bytes = serde_json::to_vec(&checkpoint).unwrap();
        journal_bytes.push(b'\n');
        fs::write(dir.path().join(JOURNAL_FILE), journal_bytes).unwrap();

        let proof = StateMigrationProof {
            schema1_profile: profile,
            request_id: "c10-migration-hold".to_string(),
            maintenance_token,
            target_kind: UpdateTargetKind::CoreRuntime,
            plan_id: plan.plan_id,
            plan_digest: plan.plan_digest,
            component_artifact_digests: plan.component_artifact_digests,
            expected_gate_generation: begin.gate_generation,
            expected_catalog_generation: catalog.generation,
        };
        (dir, proof, catalog)
    }

    #[test]
    fn active_task_rejects_apply_and_preserves_queue_state() {
        let (_dir, maintenance, request) = setup();
        maintenance
            .admit_task("cyrene-catalogs", "run-19", TaskActivityState::Queued)
            .unwrap();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(readiness.status, ReadinessStatus::ActiveTasks);
        assert_eq!(readiness.active_task_count, 1);
        let begin = maintenance
            .begin_maintenance(
                "update-19",
                &package_plan(),
                &request,
                readiness.gate_generation,
                false,
                package_usage(),
            )
            .unwrap();
        assert_eq!(begin.status, ReadinessStatus::ActiveTasks);
        assert!(begin.maintenance_token.is_none());
    }

    #[test]
    fn core_bootstrap_holds_unknown_fresh_store_and_restores_without_ready_claim() {
        let (dir, gate, request) = bootstrap_setup();
        let readiness = gate
            .get_update_readiness(&request, RuntimeUsage::default())
            .unwrap();
        assert_eq!(readiness.status, ReadinessStatus::Unknown);
        assert!(!readiness.unknown_activity_sources.is_empty());

        let begun = gate
            .begin_core_bootstrap("first-kernel", &package_plan(), &request, 0, true)
            .unwrap();
        assert_eq!(begun.status, ReadinessStatus::MaintenanceActive);
        assert!(begun.maintenance_token.is_some());
        assert_eq!(begun.gate_generation, 1);

        let reopened =
            RuntimeMaintenance::open(dir.path(), gate.inner.catalog.read().unwrap().clone())
                .unwrap();
        let restored = reopened
            .get_update_readiness(&request, RuntimeUsage::default())
            .unwrap();
        assert_eq!(restored.status, ReadinessStatus::Unknown);
        assert!(restored
            .blocker_codes
            .contains(&"RUNTIME_ACTIVITY_UNKNOWN".to_string()));
        assert!(reopened
            .with_state(|state| Ok(state
                .maintenance
                .as_ref()
                .is_some_and(|record| record.origin == MaintenanceOrigin::CoreBootstrap)))
            .unwrap());
        let held_counts = reopened
            .get_update_readiness_with(&request, || RuntimeUsage {
                known: true,
                active_worker_count: 0,
                active_allocation_count: 0,
            })
            .unwrap();
        assert_eq!(held_counts.status, ReadinessStatus::Unknown);
        assert!(held_counts
            .blocker_codes
            .contains(&"ACTIVITY_SOURCE_UNKNOWN".to_string()));
        assert!(!held_counts
            .blocker_codes
            .contains(&"RUNTIME_ACTIVITY_UNKNOWN".to_string()));
        assert!(reopened
            .admit_task("cyrene-catalogs", "blocked", TaskActivityState::Accepted)
            .is_err());
        assert!(matches!(
            reopened.with_runtime_admission_named("blocked-start", || Ok::<_, ()>(())),
            Err(RuntimeAdmissionError::Gate(MaintenanceError::AdmissionDenied(code)))
                if code == "UPDATE_MAINTENANCE_ACTIVE"
        ));
        let same = reopened
            .begin_core_bootstrap("first-kernel", &package_plan(), &request, 0, true)
            .unwrap();
        assert_eq!(same.maintenance_token, begun.maintenance_token);
        assert!(reopened
            .begin_core_bootstrap("first-kernel", &package_plan(), &request, 1, true)
            .is_err());

        let mut changed_plan = package_plan();
        changed_plan.plan_digest = format!("sha256:{}", "c".repeat(64));
        assert!(reopened
            .begin_core_bootstrap("first-kernel", &changed_plan, &request, 0, true)
            .is_err());
    }

    #[test]
    fn bootstrap_retry_rejects_changed_current_catalog_generation_or_sources() {
        for changed_generation in [true, false] {
            let (dir, gate, request) = bootstrap_setup();
            gate.begin_core_bootstrap("first-kernel", &package_plan(), &request, 0, true)
                .unwrap();
            {
                let mut catalog = gate.inner.catalog.write().unwrap();
                if changed_generation {
                    catalog.generation += 1;
                } else {
                    catalog.sources[0].source_id = "cyrene-other".to_string();
                }
            }
            let current_catalog = gate.inner.catalog.read().unwrap().clone();
            let reopened = RuntimeMaintenance::open(dir.path(), current_catalog);
            if changed_generation {
                assert!(matches!(
                    reopened,
                    Err(MaintenanceError::AdmissionDenied(code))
                        if code == "MAINTENANCE_HOLD_PROOF_REQUIRED"
                ));
                continue;
            }
            let reopened = reopened.unwrap();
            assert!(reopened
                .begin_core_bootstrap("first-kernel", &package_plan(), &request, 0, true)
                .is_err());
        }
    }

    #[test]
    fn ordinary_core_begin_still_rejects_unknown_sources_and_nonfresh_bootstrap_is_denied() {
        let (_dir, gate, bootstrap_request) = bootstrap_setup();
        let sampled = gate
            .get_update_readiness_with(&bootstrap_request, || RuntimeUsage {
                known: true,
                active_worker_count: 3,
                active_allocation_count: 2,
            })
            .unwrap();
        assert_eq!(sampled.status, ReadinessStatus::Unknown);
        assert_eq!(sampled.active_worker_count, 3);
        assert_eq!(sampled.active_allocation_count, 2);

        let ordinary = gate
            .begin_maintenance(
                "ordinary-core",
                &package_plan(),
                &bootstrap_request,
                0,
                true,
                RuntimeUsage::default(),
            )
            .unwrap();
        assert_eq!(ordinary.status, ReadinessStatus::Unknown);
        assert!(ordinary.maintenance_token.is_none());

        gate.heartbeat_activity_source("cyrene-catalogs").unwrap();
        assert!(gate
            .begin_core_bootstrap("too-late", &package_plan(), &bootstrap_request, 0, true)
            .is_err());
    }

    #[test]
    fn legacy_state_without_bootstrap_field_fails_closed() {
        let state = PersistedState::default();
        let mut value = serde_json::to_value(state).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("core_bootstrap_eligible");
        let restored: PersistedState = serde_json::from_value(value).unwrap();
        assert!(!restored.core_bootstrap_eligible);

        let mut record = serde_json::json!({
            "request_id": "old", "token": "token", "target_kind": "PACKAGE_ONLY",
            "requires_restart": false, "user_confirmed_restart": false,
            "expected_gate_generation": 0, "expected_catalog_generation": 7,
            "expected_activity_sources": [], "plan": serde_json::to_value(package_plan()).unwrap(),
            "started_at_unix_ms": 1
        });
        record.as_object_mut().unwrap().remove("origin");
        let restored: MaintenanceRecord = serde_json::from_value(record).unwrap();
        assert_eq!(restored.origin, MaintenanceOrigin::Standard);
    }

    #[test]
    fn invalid_operator_token_is_rejected() {
        let (dir, gate, _) = bootstrap_setup();
        let token = gate
            .initialize_operator_capability(dir.path().join("private/operator.token"))
            .unwrap();
        assert!(gate.verify_operator_token(&token).unwrap());
        assert!(!gate.verify_operator_token("incorrect-token").unwrap());
    }

    #[test]
    fn health_bootstrap_eligibility_is_current_and_consumed_by_authority_activity() {
        let (_dir, gate, _) = bootstrap_setup();
        assert!(gate.core_bootstrap_eligible().unwrap());

        // A current catalog refresh is observational setup and must not consume eligibility.
        gate.inner.catalog.write().unwrap().generation += 1;
        assert!(gate.core_bootstrap_eligible().unwrap());
        assert_eq!(gate.current_gate_generation().unwrap(), 0);

        gate.heartbeat_activity_source("cyrene-catalogs").unwrap();
        assert!(!gate.core_bootstrap_eligible().unwrap());
    }

    #[test]
    fn begin_and_admit_share_one_atomic_file_lock() {
        let (dir, maintenance, request) = setup();
        let preview = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(preview.status, ReadinessStatus::Ready);
        let barrier = Arc::new(Barrier::new(3));
        let begin_gate = maintenance.clone();
        let begin_request = request.clone();
        let begin_barrier = Arc::clone(&barrier);
        let begin = thread::spawn(move || {
            begin_barrier.wait();
            begin_gate
                .begin_maintenance(
                    "race-update",
                    &package_plan(),
                    &begin_request,
                    preview.gate_generation,
                    false,
                    package_usage(),
                )
                .unwrap()
        });
        let admission_gate = maintenance.clone();
        let admission_barrier = Arc::clone(&barrier);
        let admission = thread::spawn(move || {
            admission_barrier.wait();
            admission_gate.admit_task("cyrene-catalogs", "race-task", TaskActivityState::Accepted)
        });
        barrier.wait();
        let begin = begin.join().unwrap();
        let admission = admission.join().unwrap();
        assert_ne!(
            begin.maintenance_token.is_some(),
            admission.is_ok(),
            "exactly one competing admission or maintenance transition must win"
        );
        drop(maintenance);
        let reopened = setup_from(dir.path());
        let current = reopened
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert!(matches!(
            current.status,
            ReadinessStatus::ActiveTasks | ReadinessStatus::MaintenanceActive
        ));
    }

    #[test]
    fn maintenance_and_product_admission_race_across_processes() {
        let (dir, maintenance, request) = setup();
        let preview = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::gate_race_child_process_helper")
            .arg("--nocapture")
            .env("CYRENE_RUNTIME_MAINTENANCE_CHILD_DIR", dir.path())
            .spawn()
            .unwrap();
        let child_ready = dir.path().join("child.ready");
        let begin_ready = dir.path().join("begin.ready");
        let start = dir.path().join("race.start");
        let child_result = dir.path().join("child.result");

        let gate = maintenance.clone();
        let begin_ready_in_thread = begin_ready.clone();
        let start_in_thread = start.clone();
        let begin_thread = thread::spawn(move || {
            fs::write(&begin_ready_in_thread, b"ready").unwrap();
            wait_for_test_file(&start_in_thread);
            gate.begin_maintenance(
                "process-race",
                &package_plan(),
                &request,
                preview.gate_generation,
                false,
                package_usage(),
            )
            .unwrap()
        });

        wait_for_test_file(&child_ready);
        wait_for_test_file(&begin_ready);
        fs::write(&start, b"go").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "admission subprocess failed");
        let admitted = fs::read_to_string(child_result).unwrap() == "admitted";
        let begun = begin_thread.join().unwrap();
        assert_ne!(
            admitted,
            begun.maintenance_token.is_some(),
            "the shared interprocess lock must let only admission or maintenance win"
        );
    }

    #[test]
    fn gate_race_child_process_helper() {
        let Ok(state_dir) = std::env::var("CYRENE_RUNTIME_MAINTENANCE_CHILD_DIR") else {
            return;
        };
        let state_dir = Path::new(&state_dir);
        let gate = setup_from(state_dir);
        fs::write(state_dir.join("child.ready"), b"ready").unwrap();
        wait_for_test_file(&state_dir.join("race.start"));
        let result = gate.admit_task(
            "cyrene-catalogs",
            "process-race-task",
            TaskActivityState::Accepted,
        );
        fs::write(
            state_dir.join("child.result"),
            if result.is_ok() {
                "admitted"
            } else {
                "blocked"
            },
        )
        .unwrap();
    }

    fn wait_for_test_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn setup_from(path: &Path) -> RuntimeMaintenance {
        RuntimeMaintenance::open(
            path,
            TrustedActivitySourceCatalog {
                schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
                generation: 7,
                sources: vec![TrustedActivitySource {
                    source_id: "cyrene-catalogs".to_string(),
                    uid: 1001,
                    gid: Some(1000),
                    source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
                    binding_scopes: Vec::new(),
                }],
            },
        )
        .unwrap()
    }

    #[test]
    fn maintenance_gate_and_token_survive_process_reopen() {
        let (dir, maintenance, request) = setup();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        let begun = maintenance
            .begin_maintenance(
                "persistent-update",
                &package_plan(),
                &request,
                readiness.gate_generation,
                false,
                package_usage(),
            )
            .unwrap();
        let token = begun.maintenance_token.unwrap();
        let reopened = setup_from(dir.path());
        assert!(reopened.with_runtime_admission(|| Ok(())).is_err());
        let failed_end = reopened
            .end_maintenance(
                "persistent-update",
                &token,
                MaintenanceOutcome::Failed,
                false,
            )
            .unwrap();
        assert!(!failed_end.unlocked);
        assert_eq!(failed_end.status, ReadinessStatus::MaintenanceActive);
        let rolled_back = reopened
            .end_maintenance(
                "persistent-update",
                &token,
                MaintenanceOutcome::RolledBack,
                true,
            )
            .unwrap();
        assert!(rolled_back.unlocked);
    }

    #[test]
    fn journal_replays_end_receipt_and_retries_unlock_idempotently() {
        let (dir, maintenance, request) = setup();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        let begun = maintenance
            .begin_maintenance(
                "crash-recovery-update",
                &package_plan(),
                &request,
                readiness.gate_generation,
                false,
                package_usage(),
            )
            .unwrap();
        let token = begun.maintenance_token.unwrap();
        let active_snapshot = fs::read(dir.path().join(STATE_FILE)).unwrap();
        let completed = maintenance
            .end_maintenance(
                "crash-recovery-update",
                &token,
                MaintenanceOutcome::Success,
                true,
            )
            .unwrap();
        assert!(completed.unlocked);

        // Simulate a crash after the End journal fsync but before the atomic
        // state snapshot replacement reaches disk.
        fs::write(dir.path().join(STATE_FILE), active_snapshot).unwrap();
        drop(maintenance);
        let reopened = setup_from(dir.path());
        reopened
            .reconcile_activity_source("cyrene-catalogs", Vec::new())
            .unwrap();
        let retried = reopened
            .end_maintenance(
                "crash-recovery-update",
                &token,
                MaintenanceOutcome::Success,
                true,
            )
            .unwrap();
        assert!(retried.unlocked);
        assert_eq!(retried.status, ReadinessStatus::Ready);
        assert_eq!(retried, completed);

        let readiness = reopened
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(readiness.status, ReadinessStatus::Ready);
        assert!(readiness.gate_generation > retried.gate_generation);

        let mismatched_retry = reopened.end_maintenance(
            "crash-recovery-update",
            &token,
            MaintenanceOutcome::RolledBack,
            true,
        );
        assert!(matches!(
            mismatched_retry,
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "MAINTENANCE_END_REPLAY_MISMATCH"
        ));
    }

    #[test]
    fn stale_or_missing_trusted_source_is_unknown_even_if_caller_omits_it() {
        let (_dir, maintenance, mut request) = setup();
        request.expected_activity_sources.clear();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(readiness.status, ReadinessStatus::Unknown);
        assert_eq!(readiness.unknown_activity_sources, vec!["cyrene-catalogs"]);
    }

    #[test]
    fn kernel_restart_requires_explicit_unload_and_confirmation() {
        let (_dir, maintenance, mut request) = setup();
        request.target_kind = UpdateTargetKind::CoreRuntime;
        request.requires_restart = true;
        let idle = RuntimeUsage {
            known: true,
            active_worker_count: 1,
            active_allocation_count: 1,
        };
        let readiness = maintenance.get_update_readiness(&request, idle).unwrap();
        assert_eq!(readiness.status, ReadinessStatus::IdleRuntimeRequiresUnload);
        let readiness = maintenance
            .get_update_readiness(&request, RuntimeUsage::default())
            .unwrap();
        assert_eq!(readiness.status, ReadinessStatus::Unknown);
        let _ = readiness;
        let readiness = maintenance
            .get_update_readiness(
                &request,
                RuntimeUsage {
                    known: true,
                    active_worker_count: 0,
                    active_allocation_count: 0,
                },
            )
            .unwrap();
        let unconfirmed = maintenance
            .begin_maintenance(
                "unconfirmed-restart",
                &package_plan(),
                &request,
                readiness.gate_generation,
                false,
                RuntimeUsage {
                    known: true,
                    active_worker_count: 0,
                    active_allocation_count: 0,
                },
            )
            .unwrap();
        assert_eq!(
            unconfirmed.status,
            ReadinessStatus::UserConfirmationRequired
        );
    }

    #[test]
    fn binding_operation_is_a_distinct_durable_blocker_and_survives_task_reconciliation() {
        let (_dir, maintenance, request, caller, scope, other_caller, _) = binding_setup();
        let admitted = maintenance
            .admit_binding_operation("yield-activate-1", &caller, &scope)
            .unwrap();
        assert_eq!(
            admitted.catalog_generation,
            caller.expected_catalog_generation
        );
        assert!(!admitted.already_completed);

        let first_readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(
            first_readiness.status,
            ReadinessStatus::ActiveBindingOperations
        );
        assert_eq!(first_readiness.active_task_count, 0);
        assert!(first_readiness.active_tasks.is_empty());
        assert_eq!(first_readiness.active_binding_operation_count, 1);
        assert_eq!(first_readiness.active_binding_operations.len(), 1);

        maintenance
            .reconcile_activity_source("cyrene-yield", Vec::new())
            .unwrap();
        let reconciled = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(reconciled.active_binding_operation_count, 1);
        assert_eq!(reconciled.status, ReadinessStatus::ActiveBindingOperations);

        let blocked_update = maintenance
            .begin_maintenance(
                "yield-update-blocked",
                &package_plan(),
                &request,
                reconciled.gate_generation,
                false,
                package_usage(),
            )
            .unwrap();
        assert_eq!(
            blocked_update.status,
            ReadinessStatus::ActiveBindingOperations
        );
        assert!(blocked_update.maintenance_token.is_none());

        let wrong_scope = BindingOperationScope {
            installation_id: "llf.other-install".to_string(),
            ..scope.clone()
        };
        assert!(matches!(
            maintenance.complete_binding_operation(
                "yield-activate-1",
                &caller,
                &wrong_scope,
                &admitted.operation_token
            ),
            Err(MaintenanceError::AdmissionDenied(_))
        ));
        assert!(matches!(
            maintenance.complete_binding_operation(
                "yield-activate-1",
                &other_caller,
                &scope,
                &admitted.operation_token
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATION_SCOPE_UNTRUSTED"
        ));
        assert_eq!(
            maintenance
                .get_update_readiness(&request, package_usage())
                .unwrap()
                .active_binding_operation_count,
            1
        );

        let completed = maintenance
            .complete_binding_operation(
                "yield-activate-1",
                &caller,
                &scope,
                &admitted.operation_token,
            )
            .unwrap();
        assert!(completed.completed);
        let replay = maintenance
            .complete_binding_operation(
                "yield-activate-1",
                &caller,
                &scope,
                &admitted.operation_token,
            )
            .unwrap();
        assert_eq!(replay, completed);
        assert_eq!(
            maintenance
                .get_update_readiness(&request, package_usage())
                .unwrap()
                .status,
            ReadinessStatus::Ready
        );
    }

    #[test]
    fn binding_operation_admission_revalidates_generation_token_peer_and_allowlist() {
        let (_dir, maintenance, _request, caller, scope, _, _) = binding_setup();
        let mut stale_generation = caller.clone();
        stale_generation.expected_catalog_generation = 11;
        assert!(matches!(
            maintenance.admit_binding_operation("stale-generation", &stale_generation, &scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "ACTIVITY_CATALOG_GENERATION_MISMATCH"
        ));

        let mut invalid_token = caller.clone();
        invalid_token.source_token = "not-the-source-token".to_string();
        assert!(matches!(
            maintenance.admit_binding_operation("invalid-token", &invalid_token, &scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "ACTIVITY_SOURCE_AUTH_INVALID"
        ));

        let mut wrong_peer = caller.clone();
        wrong_peer.peer_uid = 0;
        assert!(matches!(
            maintenance.admit_binding_operation("wrong-peer", &wrong_peer, &scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "ACTIVITY_SOURCE_CALLER_MISMATCH"
        ));

        let untrusted_scope = BindingOperationScope {
            installation_id: "llf.unlisted-install".to_string(),
            operation: BindingOperationKind::Deactivate,
            ..scope.clone()
        };
        assert!(matches!(
            maintenance.admit_binding_operation("untrusted-scope", &caller, &untrusted_scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATION_SCOPE_UNTRUSTED"
        ));
    }

    #[test]
    fn catalog_replacement_waits_for_pending_binding_owner_completion() {
        let (_dir, maintenance, _request, caller, scope, _, _) = binding_setup();
        let admission = maintenance
            .admit_binding_operation("catalog-update-pending", &caller, &scope)
            .unwrap();
        let mut replacement = binding_test_catalog();
        replacement.generation = 13;
        replacement
            .sources
            .retain(|source| source.source_id != "cyrene-yield");
        replacement.validate().unwrap();

        let mut writer_called = false;
        let rejected =
            maintenance.commit_activity_source_catalog(12, replacement.clone(), None, || {
                writer_called = true;
                Ok(())
            });
        assert!(matches!(
            rejected,
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATIONS_INFLIGHT"
        ));
        assert!(!writer_called);
        assert_eq!(maintenance.catalog_generation(), 12);

        let completed = maintenance
            .complete_binding_operation(
                "catalog-update-pending",
                &caller,
                &scope,
                &admission.operation_token,
            )
            .unwrap();
        assert!(completed.completed);

        maintenance
            .commit_activity_source_catalog(12, replacement, None, || {
                writer_called = true;
                Ok(())
            })
            .unwrap();
        assert!(writer_called);
        assert_eq!(maintenance.catalog_generation(), 13);
    }

    #[test]
    fn catalog_replacement_fails_closed_without_clearing_pending_owner() {
        let (dir, maintenance, request, caller, scope, _, _) = binding_setup();
        let original_catalog = binding_test_catalog();
        let admission = maintenance
            .admit_binding_operation("external-catalog-change", &caller, &scope)
            .unwrap();

        let mut untrusted_replacement = original_catalog.clone();
        untrusted_replacement.generation = 13;
        untrusted_replacement
            .sources
            .retain(|source| source.source_id != caller.source_id);
        *maintenance.inner.catalog.write().unwrap() = untrusted_replacement.clone();
        assert!(matches!(
            maintenance.refresh_catalog(),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATIONS_INFLIGHT"
        ));
        drop(maintenance);

        assert!(matches!(
            RuntimeMaintenance::open(dir.path(), untrusted_replacement),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATIONS_INFLIGHT"
        ));

        // Restoring the authoritative catalog permits only the real owner to
        // complete; the failed replacement did not auto-clear the reservation.
        let restored = RuntimeMaintenance::open(dir.path(), original_catalog).unwrap();
        let readiness = restored
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(readiness.active_binding_operation_count, 1);
        assert!(
            restored
                .complete_binding_operation(
                    "external-catalog-change",
                    &caller,
                    &scope,
                    &admission.operation_token,
                )
                .unwrap()
                .completed
        );
    }

    #[test]
    fn exact_hold_proof_allows_catalog_commit_without_invalidating_hold_validation() {
        let (dir, maintenance, request, _, _, _, _) = binding_setup();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        let plan = package_plan();
        let request_id = "llf-package-install";
        let begun = maintenance
            .begin_maintenance(
                request_id,
                &plan,
                &request,
                readiness.gate_generation,
                false,
                package_usage(),
            )
            .unwrap();
        assert_eq!(begun.status, ReadinessStatus::MaintenanceActive);
        let token = begun.maintenance_token.unwrap();
        let held_gate_generation = begun.gate_generation;
        let artifact_digest = plan.component_artifact_digests["cyrene-yield"].clone();
        let proof_for = |maintenance_token: &str, plan: &MaintenancePlan| MaintenanceHoldProof {
            request_id: request_id.to_string(),
            maintenance_token: maintenance_token.to_string(),
            plan: plan.clone(),
        };
        let hold_proof = proof_for(&token, &plan);

        let before = maintenance
            .validate_maintenance_hold(
                &hold_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation,
                12,
            )
            .unwrap();
        assert!(before.valid);
        assert_eq!(before.catalog_generation, 12);
        assert_eq!(before.gate_generation, held_gate_generation);
        let projected = serde_json::to_value(&before).unwrap();
        assert_eq!(projected["valid"], true);
        assert_eq!(projected["target_kind"], "PACKAGE_ONLY");
        assert_eq!(projected["plan_id"], plan.plan_id);
        assert_eq!(projected["component_id"], "cyrene-yield");
        assert_eq!(projected["artifact_digest"], artifact_digest);
        assert!(projected.get("plan").is_none());
        assert_eq!(
            maintenance.current_gate_generation().unwrap(),
            held_gate_generation
        );
        assert!(matches!(
            maintenance.validate_maintenance_hold(
                &proof_for("wrong-token", &plan),
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation,
                12,
            ),
            Err(MaintenanceError::AdmissionDenied(code)) if code == "MAINTENANCE_TOKEN_INVALID"
        ));
        assert!(matches!(
            maintenance.validate_maintenance_hold(
                &hold_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation.saturating_add(1),
                12,
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "MAINTENANCE_GATE_GENERATION_MISMATCH"
        ));
        let mut different_plan = plan.clone();
        different_plan.plan_id.push_str("-different");
        let different_plan_proof = proof_for(&token, &different_plan);
        assert!(matches!(
            maintenance.validate_maintenance_hold(
                &different_plan_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation,
                12,
            ),
            Err(MaintenanceError::AdmissionDenied(code)) if code == "MAINTENANCE_PLAN_MISMATCH"
        ));
        assert!(matches!(
            maintenance.validate_maintenance_hold(
                &hold_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                "sha256:wrong",
                held_gate_generation,
                12,
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "MAINTENANCE_COMPONENT_DIGEST_MISMATCH"
        ));

        let mut replacement = binding_test_catalog();
        replacement.generation = 13;
        replacement.sources[1].binding_scopes[0]
            .installation_ids
            .push("llf.install.receipt-9".to_string());
        let proof = MaintenanceHoldProof {
            request_id: request_id.to_string(),
            maintenance_token: token.clone(),
            plan: plan.clone(),
        };

        let mut writer_called = false;
        assert!(matches!(
            maintenance.commit_activity_source_catalog(
                12,
                replacement.clone(),
                None,
                || {
                    writer_called = true;
                    Ok(())
                }
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "MAINTENANCE_HOLD_PROOF_REQUIRED"
        ));
        assert!(!writer_called);

        let wrong_proof = MaintenanceHoldProof {
            request_id: request_id.to_string(),
            maintenance_token: "wrong-token".to_string(),
            plan: plan.clone(),
        };
        assert!(matches!(
            maintenance.commit_activity_source_catalog(
                12,
                replacement.clone(),
                Some(&wrong_proof),
                || {
                    writer_called = true;
                    Ok(())
                }
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "MAINTENANCE_HOLD_PROOF_MISMATCH"
        ));
        assert!(!writer_called);

        let replacement_for_restart = replacement.clone();
        maintenance
            .commit_activity_source_catalog(12, replacement, Some(&proof), || {
                writer_called = true;
                Ok(())
            })
            .unwrap();
        assert!(writer_called);
        drop(maintenance);
        let maintenance = RuntimeMaintenance::open(dir.path(), replacement_for_restart).unwrap();
        assert_eq!(
            maintenance.current_gate_generation().unwrap(),
            held_gate_generation
        );
        assert!(matches!(
            maintenance.validate_maintenance_hold(
                &hold_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation,
                12,
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "ACTIVITY_CATALOG_GENERATION_MISMATCH"
        ));
        let after = maintenance
            .validate_maintenance_hold(
                &hold_proof,
                UpdateTargetKind::PackageOnly,
                "cyrene-yield",
                &artifact_digest,
                held_gate_generation,
                13,
            )
            .unwrap();
        assert!(after.valid);
        assert_eq!(after.catalog_generation, 13);
        assert_eq!(after.gate_generation, held_gate_generation);

        let ended = maintenance
            .end_maintenance(request_id, &token, MaintenanceOutcome::Success, true)
            .unwrap();
        assert!(ended.unlocked);
    }

    #[test]
    fn catalog_commit_races_owner_completion_under_the_shared_gate_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_dir, maintenance, _request, caller, scope, _, _) = binding_setup();
        let admission = maintenance
            .admit_binding_operation("catalog-complete-race", &caller, &scope)
            .unwrap();
        let mut replacement = binding_test_catalog();
        replacement.generation = 13;
        replacement
            .sources
            .retain(|source| source.source_id != "cyrene-yield");
        let barrier = Arc::new(Barrier::new(3));
        let writer_called = Arc::new(AtomicBool::new(false));

        let update_gate = maintenance.clone();
        let update_barrier = Arc::clone(&barrier);
        let update_writer_called = Arc::clone(&writer_called);
        let update = thread::spawn(move || {
            update_barrier.wait();
            update_gate.commit_activity_source_catalog(12, replacement, None, || {
                update_writer_called.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

        let complete_gate = maintenance.clone();
        let complete_barrier = Arc::clone(&barrier);
        let complete = thread::spawn(move || {
            complete_barrier.wait();
            complete_gate.complete_binding_operation(
                "catalog-complete-race",
                &caller,
                &scope,
                &admission.operation_token,
            )
        });
        barrier.wait();

        let update_result = update.join().unwrap();
        assert!(complete.join().unwrap().unwrap().completed);
        match update_result {
            Ok(()) => {
                assert!(writer_called.load(Ordering::SeqCst));
                assert_eq!(maintenance.catalog_generation(), 13);
            }
            Err(MaintenanceError::AdmissionDenied(code)) => {
                assert_eq!(code, "BINDING_OPERATIONS_INFLIGHT");
                assert!(!writer_called.load(Ordering::SeqCst));
                assert_eq!(maintenance.catalog_generation(), 12);
            }
            Err(error) => panic!("unexpected catalog update error: {error}"),
        }
    }

    #[test]
    fn legacy_catalogs_default_to_no_binding_authority() {
        let source: TrustedActivitySource = serde_json::from_value(serde_json::json!({
            "source_id": "cyrene-catalogs",
            "uid": 1001,
            "gid": 1000,
            "source_token_sha256": format!("{:x}", Sha256::digest(b"test-source-token")),
        }))
        .unwrap();
        assert!(source.binding_scopes.is_empty());

        let (_dir, maintenance, _request) = setup();
        let caller = BindingOperationCaller {
            source_id: "cyrene-catalogs".to_string(),
            source_token: "test-source-token".to_string(),
            peer_uid: 1001,
            peer_gid: 1000,
            expected_catalog_generation: 7,
        };
        let scope = BindingOperationScope {
            binding_id: "binding-main".to_string(),
            package_id: "llf.trainer".to_string(),
            installation_id: "llf.install.2026".to_string(),
            operation: BindingOperationKind::Activate,
        };
        assert!(matches!(
            maintenance.admit_binding_operation("legacy-catalog-denied", &caller, &scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATION_SCOPE_UNTRUSTED"
        ));

        let mut invalid_catalog = binding_test_catalog();
        invalid_catalog.sources[0].binding_scopes[0]
            .installation_ids
            .clear();
        assert!(matches!(
            invalid_catalog.validate(),
            Err(MaintenanceError::CatalogUnavailable(_))
        ));
    }

    #[test]
    fn trusted_catalog_rejects_binding_ownership_shared_between_sources() {
        let mut catalog = binding_test_catalog();
        let mut second_owner = catalog.sources[0].clone();
        second_owner.source_id = "cyrene-third".to_string();
        second_owner.binding_scopes[0].binding_id = "binding-main".to_string();
        catalog.sources.push(second_owner);
        assert!(matches!(
            catalog.validate(),
            Err(MaintenanceError::CatalogUnavailable(message))
                if message.contains("authorized by more than one source")
        ));
    }

    #[test]
    fn binding_operation_request_id_is_scoped_idempotently_across_sources() {
        let (_dir, maintenance, _request, caller, scope, other_caller, other_scope) =
            binding_setup();
        let first = maintenance
            .admit_binding_operation("shared-request-id", &caller, &scope)
            .unwrap();
        let retry = maintenance
            .admit_binding_operation("shared-request-id", &caller, &scope)
            .unwrap();
        assert_eq!(retry.operation_token, first.operation_token);
        assert_eq!(retry.catalog_generation, caller.expected_catalog_generation);
        assert_eq!(retry.gate_generation, first.gate_generation);
        assert!(retry.already_in_flight);
        assert!(!retry.already_completed);

        assert!(matches!(
            maintenance.admit_binding_operation("shared-request-id", &other_caller, &other_scope),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATION_REQUEST_ID_CONFLICT"
        ));

        let completed = maintenance
            .complete_binding_operation(
                "shared-request-id",
                &caller,
                &scope,
                &first.operation_token,
            )
            .unwrap();
        assert!(completed.completed);
        let completed_retry = maintenance
            .admit_binding_operation("shared-request-id", &caller, &scope)
            .unwrap();
        assert!(completed_retry.already_completed);
        assert_eq!(completed_retry.operation_token, first.operation_token);
    }

    #[test]
    fn binding_operation_new_request_id_cannot_overlap_same_binding() {
        let (_dir, maintenance, request, caller, scope, other_caller, other_scope) =
            binding_setup();
        let first = maintenance
            .admit_binding_operation("binding-first-id", &caller, &scope)
            .unwrap();
        let retried_intent_scope = BindingOperationScope {
            operation: BindingOperationKind::Recover,
            ..scope.clone()
        };
        assert!(matches!(
            maintenance.admit_binding_operation(
                "binding-new-id",
                &caller,
                &retried_intent_scope,
            ),
            Err(MaintenanceError::AdmissionDenied(code))
                if code == "BINDING_OPERATION_ALREADY_INFLIGHT"
        ));
        assert_eq!(
            maintenance.current_gate_generation().unwrap(),
            first.gate_generation
        );
        assert_eq!(
            maintenance
                .get_update_readiness(&request, package_usage())
                .unwrap()
                .active_binding_operation_count,
            1
        );

        let different_binding = maintenance
            .admit_binding_operation("other-binding-id", &other_caller, &other_scope)
            .unwrap();
        assert_eq!(
            maintenance
                .get_update_readiness(&request, package_usage())
                .unwrap()
                .active_binding_operation_count,
            2
        );

        assert!(
            maintenance
                .complete_binding_operation(
                    "binding-first-id",
                    &caller,
                    &scope,
                    &first.operation_token,
                )
                .unwrap()
                .completed
        );
        let retry_with_new_id = maintenance
            .admit_binding_operation("binding-new-id", &caller, &retried_intent_scope)
            .unwrap();
        assert!(!retry_with_new_id.already_in_flight);
        assert!(!retry_with_new_id.already_completed);
        assert!(
            maintenance
                .complete_binding_operation(
                    "binding-new-id",
                    &caller,
                    &retried_intent_scope,
                    &retry_with_new_id.operation_token,
                )
                .unwrap()
                .completed
        );
        assert!(
            maintenance
                .complete_binding_operation(
                    "other-binding-id",
                    &other_caller,
                    &other_scope,
                    &different_binding.operation_token,
                )
                .unwrap()
                .completed
        );
    }

    #[test]
    fn binding_operation_crash_pending_survives_reopen_and_has_no_ttl_release() {
        let (dir, maintenance, request, caller, scope, _, _) = binding_setup();
        let admitted = maintenance
            .admit_binding_operation("crash-pending", &caller, &scope)
            .unwrap();
        drop(maintenance);

        let reopened = RuntimeMaintenance::open(dir.path(), binding_test_catalog()).unwrap();
        let readiness = reopened
            .get_update_readiness(&request, package_usage())
            .unwrap();
        assert_eq!(readiness.active_binding_operation_count, 1);
        assert_eq!(readiness.status, ReadinessStatus::ActiveBindingOperations);

        let idempotent = reopened
            .admit_binding_operation("crash-pending", &caller, &scope)
            .unwrap();
        assert_eq!(idempotent.operation_token, admitted.operation_token);
        assert!(!idempotent.already_completed);
        assert!(
            reopened
                .complete_binding_operation(
                    "crash-pending",
                    &caller,
                    &scope,
                    &admitted.operation_token,
                )
                .unwrap()
                .completed
        );
        assert_eq!(
            reopened
                .get_update_readiness(&request, package_usage())
                .unwrap()
                .status,
            ReadinessStatus::Ready
        );
    }

    #[test]
    fn binding_admission_and_update_begin_have_one_atomic_winner() {
        let (_dir, maintenance, request, caller, scope, _, _) = binding_setup();
        let readiness = maintenance
            .get_update_readiness(&request, package_usage())
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let operation_gate = maintenance.clone();
        let operation_barrier = Arc::clone(&barrier);
        let operation = thread::spawn(move || {
            operation_barrier.wait();
            operation_gate.admit_binding_operation("race-operation", &caller, &scope)
        });

        let update_gate = maintenance.clone();
        let update_barrier = Arc::clone(&barrier);
        let update_request = request.clone();
        let update = thread::spawn(move || {
            update_barrier.wait();
            update_gate.begin_maintenance(
                "race-update",
                &package_plan(),
                &update_request,
                readiness.gate_generation,
                false,
                package_usage(),
            )
        });
        barrier.wait();

        let operation = operation.join().unwrap();
        let update = update.join().unwrap().unwrap();
        let operation_won = operation.is_ok();
        let update_won = update.maintenance_token.is_some();
        assert_ne!(operation_won, update_won);
        if operation_won {
            assert!(matches!(
                update.status,
                ReadinessStatus::ActiveBindingOperations | ReadinessStatus::StaleReadiness
            ));
            assert!(update.maintenance_token.is_none());
        } else {
            assert_eq!(update.status, ReadinessStatus::MaintenanceActive);
            assert!(matches!(
                operation,
                Err(MaintenanceError::AdmissionDenied(code))
                    if code == "UPDATE_MAINTENANCE_ACTIVE"
            ));
        }
    }

    #[test]
    fn ordinary_open_refuses_released_schema1_without_rewriting_files() {
        let (dir, _proof, catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        let state_before = fs::read(dir.path().join(STATE_FILE)).unwrap();
        let journal_before = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();

        assert!(RuntimeMaintenance::open(dir.path(), catalog).is_err());
        assert_eq!(fs::read(dir.path().join(STATE_FILE)).unwrap(), state_before);
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
            journal_before
        );
    }

    #[test]
    fn controlled_schema1_migration_preserves_both_supported_profiles() {
        for profile in [
            LegacyStateProfile::ReleasedV1NoBindingAdmissions,
            LegacyStateProfile::ExperimentalV1BindingAdmissions,
        ] {
            let (dir, proof, catalog) = legacy_migration_fixture(profile);
            let source_state = fs::read(dir.path().join(STATE_FILE)).unwrap();
            let source_journal = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();
            let mut wrong_profile = proof.clone();
            wrong_profile.schema1_profile = match profile {
                LegacyStateProfile::ReleasedV1NoBindingAdmissions => {
                    LegacyStateProfile::ExperimentalV1BindingAdmissions
                }
                LegacyStateProfile::ExperimentalV1BindingAdmissions => {
                    LegacyStateProfile::ReleasedV1NoBindingAdmissions
                }
            };
            assert!(migrate_state_schema1(dir.path(), &wrong_profile).is_err());
            assert_eq!(fs::read(dir.path().join(STATE_FILE)).unwrap(), source_state);
            assert_eq!(
                fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
                source_journal
            );

            let result = migrate_state_schema1(dir.path(), &proof).unwrap();
            assert_eq!(result.schema_version, STATE_SCHEMA_VERSION);
            assert!(result.migrated);
            let state =
                load_state_locked_with_schema(dir.path(), STATE_SCHEMA_VERSION, None, false)
                    .unwrap();
            assert!(held_state_matches_marker(
                &state,
                &read_migration_marker(dir.path()).unwrap()
            ));
            assert_eq!(
                state
                    .completed_binding_operations
                    .contains_key("completed-before-c10"),
                profile == LegacyStateProfile::ExperimentalV1BindingAdmissions
            );
            assert!(load_state_locked_with_schema(
                dir.path(),
                LEGACY_STATE_SCHEMA_VERSION,
                Some(profile),
                false
            )
            .is_err());
            assert!(RuntimeMaintenance::open(dir.path(), catalog).is_ok());
        }
    }

    #[test]
    fn complete_marker_allows_normal_state_and_held_catalog_writes() {
        let (dir, proof, catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        migrate_state_schema1(dir.path(), &proof).unwrap();

        let gate = RuntimeMaintenance::open(dir.path(), catalog.clone()).unwrap();
        gate.heartbeat_activity_source("cyrene-kernel").unwrap();
        let hold_proof = MaintenanceHoldProof {
            request_id: proof.request_id.clone(),
            maintenance_token: proof.maintenance_token.clone(),
            plan: MaintenancePlan {
                plan_id: proof.plan_id.clone(),
                plan_digest: proof.plan_digest.clone(),
                component_artifact_digests: proof.component_artifact_digests.clone(),
            },
        };
        let replacement = TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation: catalog.generation + 1,
            sources: catalog.sources.clone(),
        };
        gate.commit_activity_source_catalog(
            catalog.generation,
            replacement.clone(),
            Some(&hold_proof),
            || Ok(()),
        )
        .unwrap();
        drop(gate);

        let reopened = RuntimeMaintenance::open(dir.path(), replacement).unwrap();
        reopened.heartbeat_activity_source("cyrene-kernel").unwrap();
        assert_eq!(reopened.catalog_generation(), 8);
        let live =
            load_state_locked_with_schema(dir.path(), STATE_SCHEMA_VERSION, None, false).unwrap();
        assert_eq!(live.install_catalog_generation, 8);
        assert_eq!(
            read_migration_marker(dir.path()).unwrap().phase,
            StateMigrationPhase::Complete
        );

        let resumed = migrate_state_schema1(dir.path(), &proof).unwrap();
        assert!(resumed.migrated);
        assert_eq!(resumed.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(resumed.gate_generation, live.gate_generation);
        assert_eq!(resumed.catalog_generation, live.install_catalog_generation);

        let state_before = fs::read(dir.path().join(STATE_FILE)).unwrap();
        let journal_before = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();
        let mut wrong_proof = proof.clone();
        wrong_proof.maintenance_token = "wrong-maintenance-token".to_string();
        assert!(migrate_state_schema1(dir.path(), &wrong_proof).is_err());
        assert_eq!(fs::read(dir.path().join(STATE_FILE)).unwrap(), state_before);
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
            journal_before
        );
    }

    #[test]
    fn completed_migration_resume_rejects_lost_hold_and_corrupt_artifacts() {
        let (dir, proof, catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        migrate_state_schema1(dir.path(), &proof).unwrap();
        let gate = RuntimeMaintenance::open(dir.path(), catalog).unwrap();
        let ended = gate
            .end_maintenance(
                &proof.request_id,
                &proof.maintenance_token,
                MaintenanceOutcome::Success,
                true,
            )
            .unwrap();
        assert!(ended.unlocked);
        drop(gate);
        assert!(migrate_state_schema1(dir.path(), &proof).is_err());

        let (dir, proof, _catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        migrate_state_schema1(dir.path(), &proof).unwrap();
        let mut marker = read_migration_marker(dir.path()).unwrap();
        marker.plan_id = "tampered-plan".to_string();
        write_migration_marker(dir.path(), &marker).unwrap();
        assert!(migrate_state_schema1(dir.path(), &proof).is_err());

        let (dir, proof, _catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        migrate_state_schema1(dir.path(), &proof).unwrap();
        let marker = read_migration_marker(dir.path()).unwrap();
        let backup_path = marker.backup_schema2_state_path(dir.path());
        let mut backup = read_private_backup(&backup_path).unwrap();
        backup.push(b' ');
        fs::write(backup_path, backup).unwrap();
        assert!(migrate_state_schema1(dir.path(), &proof).is_err());
    }

    #[test]
    fn migration_rejects_partial_or_unknown_schema1_fields_without_mutation() {
        let (dir, mut proof, _catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        let state_path = dir.path().join(STATE_FILE);
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state
            .as_object_mut()
            .unwrap()
            .insert("binding_operations".to_string(), serde_json::json!({}));
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        proof.schema1_profile = LegacyStateProfile::ExperimentalV1BindingAdmissions;
        let state_before = fs::read(&state_path).unwrap();
        let journal_before = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();
        assert!(migrate_state_schema1(dir.path(), &proof).is_err());
        assert_eq!(fs::read(&state_path).unwrap(), state_before);
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
            journal_before
        );

        let (dir, proof, _catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        let state_path = dir.path().join(STATE_FILE);
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state.as_object_mut().unwrap().insert(
            "unrecognized_field".to_string(),
            serde_json::Value::Bool(true),
        );
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let state_before = fs::read(&state_path).unwrap();
        let journal_before = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();
        assert!(migrate_state_schema1(dir.path(), &proof).is_err());
        assert_eq!(fs::read(&state_path).unwrap(), state_before);
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
            journal_before
        );
    }

    #[test]
    fn migration_resumes_only_known_marker_hash_phase_and_old_writer_rejects_v2() {
        let (dir, proof, catalog) =
            legacy_migration_fixture(LegacyStateProfile::ReleasedV1NoBindingAdmissions);
        migrate_state_schema1(dir.path(), &proof).unwrap();
        let mut marker = read_migration_marker(dir.path()).unwrap();
        marker.phase = StateMigrationPhase::Prepared;
        write_migration_marker(dir.path(), &marker).unwrap();
        fs::write(
            dir.path().join(STATE_FILE),
            read_private_backup(&marker.backup_state_path(dir.path())).unwrap(),
        )
        .unwrap();
        assert!(RuntimeMaintenance::open(dir.path(), catalog).is_err());

        let resumed = migrate_state_schema1(dir.path(), &proof).unwrap();
        assert_eq!(resumed.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(
            read_migration_marker(dir.path()).unwrap().phase,
            StateMigrationPhase::Complete
        );
        assert!(load_state_locked_with_schema(
            dir.path(),
            LEGACY_STATE_SCHEMA_VERSION,
            Some(proof.schema1_profile),
            false
        )
        .is_err());

        let before_state = fs::read(dir.path().join(STATE_FILE)).unwrap();
        let before_journal = fs::read(dir.path().join(JOURNAL_FILE)).unwrap();
        fs::write(dir.path().join(JOURNAL_FILE), b"tampered unknown journal\n").unwrap();
        assert!(rollback_state_schema1(dir.path(), &proof).is_err());
        assert_eq!(fs::read(dir.path().join(STATE_FILE)).unwrap(), before_state);
        assert_ne!(
            fs::read(dir.path().join(JOURNAL_FILE)).unwrap(),
            before_journal
        );
        assert!(fs::symlink_metadata(dir.path().join(STATE_MIGRATION_MARKER_FILE)).is_ok());
    }

    #[test]
    fn failed_first_schema2_boot_rolls_back_after_real_kernel_reconciliation() {
        let profile = LegacyStateProfile::ExperimentalV1BindingAdmissions;
        let (dir, proof, catalog) = legacy_migration_fixture(profile);
        migrate_state_schema1(dir.path(), &proof).unwrap();
        let migrated =
            load_state_locked_with_schema(dir.path(), STATE_SCHEMA_VERSION, None, false).unwrap();
        let old_receipt = migrated
            .completed_binding_operations
            .get("completed-before-c10")
            .unwrap()
            .clone();

        // Exercise the real empty startup reconciliation while the hold is active.
        let candidate_kernel = RuntimeMaintenance::open(dir.path(), catalog).unwrap();
        candidate_kernel
            .reconcile_activity_source("cyrene-kernel", Vec::new())
            .unwrap();
        candidate_kernel
            .heartbeat_activity_source("cyrene-kernel")
            .unwrap();
        let candidate_state =
            load_state_locked_with_schema(dir.path(), STATE_SCHEMA_VERSION, None, false).unwrap();
        assert!(candidate_state.journal_sequence > migrated.journal_sequence);
        assert!(candidate_state.gate_generation > proof.expected_gate_generation);
        assert!(candidate_state.tasks.is_empty());
        assert!(candidate_state.runtime_admissions.is_empty());
        assert!(candidate_state.binding_operations.is_empty());
        drop(candidate_kernel);

        // Model failed candidate health after all writers have been stopped.
        let rollback = rollback_state_schema1(dir.path(), &proof).unwrap();
        assert_eq!(rollback.schema_version, LEGACY_STATE_SCHEMA_VERSION);
        let restored = load_state_locked_with_schema(
            dir.path(),
            LEGACY_STATE_SCHEMA_VERSION,
            Some(profile),
            false,
        )
        .unwrap();
        assert_eq!(restored.gate_generation, candidate_state.gate_generation);
        assert_eq!(
            restored.install_catalog_generation,
            candidate_state.install_catalog_generation
        );
        assert!(restored.tasks.is_empty());
        assert!(restored.runtime_admissions.is_empty());
        assert!(restored.binding_operations.is_empty());
        assert_eq!(
            restored
                .completed_binding_operations
                .get("completed-before-c10"),
            Some(&old_receipt)
        );
        assert_eq!(
            restored
                .maintenance
                .as_ref()
                .map(|active| active.request_id.as_str()),
            Some("c10-migration-hold")
        );
        assert!(fs::symlink_metadata(dir.path().join(STATE_MIGRATION_MARKER_FILE)).is_err());
    }

    // ------------------------------------------------------------------
    // F-8 regression: first-boot catalog provisioning must not crash-loop
    // the maintenance broker, while corrupt/weak-permission catalogs and
    // post-provision deletion stay fail-closed.
    // ------------------------------------------------------------------

    fn running_as_root() -> bool {
        // Root-only file tests need a genuinely privileged process: the
        // effective UID decides whether the root-owned catalog checks can
        // pass. A skip is logged so CI evidence shows which tests ran.
        let privileged = nix::unistd::geteuid().as_raw() == 0;
        if !privileged {
            eprintln!(
                "skipping root-only catalog file tests: effective uid is {} (requires uid 0)",
                nix::unistd::geteuid().as_raw()
            );
        }
        privileged
    }

    fn write_catalog_file(path: &Path, catalog: &TrustedActivitySourceCatalog) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let bytes = serde_json::to_vec_pretty(catalog).unwrap();
        fs::write(path, bytes).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn single_source_catalog(generation: u64) -> TrustedActivitySourceCatalog {
        TrustedActivitySourceCatalog {
            schema_version: ACTIVITY_SOURCE_CATALOG_SCHEMA_VERSION,
            generation,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-catalogs".to_string(),
                uid: 1001,
                gid: Some(1000),
                source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
                binding_scopes: Vec::new(),
            }],
        }
    }

    #[test]
    fn unprovisioned_missing_catalog_starts_fail_closed() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        let gate =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(gate.catalog_generation(), 0);
        // No source is trusted before provisioning: activity admissions fail closed.
        assert!(gate.heartbeat_activity_source("cyrene-catalogs").is_err());
        // Generation comparisons fail closed: gen 0 cannot satisfy gen 1, and
        // the empty catalog trusts no activity source.
        let readiness = ReadinessRequest {
            target_kind: UpdateTargetKind::PackageOnly,
            requires_restart: false,
            expected_catalog_generation: 1,
            expected_activity_sources: vec!["cyrene-catalogs".to_string()],
        };
        let snapshot = gate
            .get_update_readiness(&readiness, RuntimeUsage::default())
            .unwrap();
        assert_eq!(snapshot.status, ReadinessStatus::Unknown);
        assert!(snapshot
            .blocker_codes
            .iter()
            .any(|code| code == "ACTIVITY_SOURCE_UNKNOWN"));
    }

    #[test]
    fn unprovisioned_broker_adopts_catalog_after_init() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        let gate =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(gate.catalog_generation(), 0);
        write_catalog_file(&catalog_path, &single_source_catalog(1));
        gate.refresh_catalog().unwrap();
        assert_eq!(gate.catalog_generation(), 1);
        gate.heartbeat_activity_source("cyrene-catalogs").unwrap();
        // Reopen adopts the provisioned catalog directly.
        drop(gate);
        let reopened =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(reopened.catalog_generation(), 1);
    }

    #[test]
    fn corrupt_catalog_file_fails_open() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        fs::write(&catalog_path, b"{not json").unwrap();
        let error = RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path)
            .err()
            .expect("corrupt catalog must fail open");
        assert!(matches!(error, MaintenanceError::CatalogUnavailable(_)));
    }

    #[test]
    fn group_writable_catalog_file_fails_open() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        write_catalog_file(&catalog_path, &single_source_catalog(1));
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o664)).unwrap();
        let error = RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path)
            .err()
            .expect("group-writable catalog must fail open");
        assert!(matches!(error, MaintenanceError::CatalogUnavailable(_)));
    }

    #[test]
    fn provisioned_then_deleted_catalog_fails_closed() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        write_catalog_file(&catalog_path, &single_source_catalog(1));
        let gate =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(gate.catalog_generation(), 1);
        // Deleting a provisioned catalog is a fail-closed event, not a
        // regression to the unprovisioned first-boot state.
        fs::remove_file(&catalog_path).unwrap();
        assert!(gate.refresh_catalog().is_err());
        drop(gate);
        assert!(
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).is_err()
        );
    }

    #[test]
    fn concurrent_optional_opens_recover_locks() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        let barrier = std::sync::Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let directory = dir.path().to_path_buf();
                let path = catalog_path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let gate =
                        RuntimeMaintenance::open_with_optional_catalog_file(directory, &path)
                            .unwrap();
                    assert_eq!(gate.catalog_generation(), 0);
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        // Abnormal-exit recovery analog: the lock is released when the holder
        // is dropped, so a fresh broker can take over immediately.
        let gate =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(gate.catalog_generation(), 0);
    }

    #[test]
    fn dangling_symlink_catalog_fails_closed() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &catalog_path).unwrap();
        // A dangling symlink is an anomaly, not an absent catalog: it must not
        // be misread as the unprovisioned first-boot state.
        assert!(
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).is_err()
        );
        // The same holds mid-flight: a provisioned broker whose catalog file
        // is replaced by a dangling symlink fails closed on reload instead of
        // regressing to the unprovisioned state.
        fs::remove_file(&catalog_path).unwrap();
        write_catalog_file(&catalog_path, &single_source_catalog(1));
        let gate =
            RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path).unwrap();
        assert_eq!(gate.catalog_generation(), 1);
        fs::remove_file(&catalog_path).unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &catalog_path).unwrap();
        assert!(gate.refresh_catalog().is_err());
    }

    #[test]
    fn directory_at_catalog_path_fails_closed() {
        if !running_as_root() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let catalog_path = dir.path().join("activity-sources.json");
        fs::create_dir_all(&catalog_path).unwrap();
        let error = RuntimeMaintenance::open_with_optional_catalog_file(dir.path(), &catalog_path)
            .err()
            .expect("a directory at the catalog path must fail open");
        assert!(matches!(error, MaintenanceError::CatalogUnavailable(_)));
    }
}
