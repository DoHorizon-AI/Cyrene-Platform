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
const OPERATOR_TOKEN_HASH_FILE: &str = "operator-token.sha256";
const STATE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_SOURCE_STALENESS: Duration = Duration::from_secs(30);
const JOURNAL_COMPACTION_THRESHOLD: u64 = 4 * 1024 * 1024;

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

    /// Validates schema, source identifiers, uniqueness, and generation.
    pub fn validate(&self) -> Result<(), MaintenanceError> {
        if self.schema_version != STATE_SCHEMA_VERSION || self.generation == 0 {
            return Err(MaintenanceError::CatalogUnavailable(
                "unsupported source catalog schema or zero generation".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
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
struct PersistedState {
    schema_version: u32,
    journal_sequence: u64,
    gate_generation: u64,
    install_catalog_generation: u64,
    maintenance: Option<MaintenanceRecord>,
    completed_maintenances: BTreeMap<String, CompletedMaintenanceRecord>,
    tasks: BTreeMap<String, TaskRecord>,
    runtime_admissions: BTreeMap<String, String>,
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
            sources: BTreeMap::new(),
            core_bootstrap_eligible: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
struct CompletedMaintenanceRecord {
    maintenance: MaintenanceRecord,
    outcome: MaintenanceOutcome,
    healthy: bool,
    unlocked: bool,
    ended_at_unix_ms: u64,
    gate_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskRecord {
    admission: TaskAdmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceRecord {
    last_heartbeat_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalEntry {
    sequence: u64,
    event: JournalEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
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
        Self::open_inner(directory.into(), catalog, None)
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
        Self::open_inner(directory.into(), catalog, Some(catalog_path))
    }

    fn open_inner(
        directory: PathBuf,
        catalog: TrustedActivitySourceCatalog,
        catalog_path: Option<PathBuf>,
    ) -> Result<Self, MaintenanceError> {
        catalog.validate()?;
        if !directory.exists() {
            fs::create_dir_all(&directory)?;
        }
        reject_symlink_directory(&directory)?;
        set_shared_directory(&directory)?;
        validate_shared_files(&directory)?;
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
        let catalog = TrustedActivitySourceCatalog::load(path)?;
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
        ReadinessStatus::ActiveTasks
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
            state.sources.insert(
                source_id.clone(),
                SourceRecord {
                    last_heartbeat_unix_ms: *at_unix_ms,
                },
            );
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
            state.sources.insert(
                source_id.clone(),
                SourceRecord {
                    last_heartbeat_unix_ms: *at_unix_ms,
                },
            );
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

fn load_state_locked(directory: &Path) -> Result<PersistedState, MaintenanceError> {
    let state_path = directory.join(STATE_FILE);
    let journal_path = directory.join(JOURNAL_FILE);
    let state_existed = state_path.exists();
    let mut state = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice::<PersistedState>(&bytes).map_err(|error| {
            MaintenanceError::StateUnknown(format!("snapshot parse failed: {error}"))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
        Err(error) => return Err(MaintenanceError::Storage(error)),
    };
    if state.schema_version != STATE_SCHEMA_VERSION {
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
                let file = OpenOptions::new().write(true).open(&journal_path)?;
                file.set_len(valid_bytes as u64)?;
                file.sync_all()?;
                break;
            }
            Err(error) => {
                return Err(MaintenanceError::StateUnknown(format!(
                    "malformed maintenance journal record: {error}"
                )));
            }
        };

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
                        || checkpoint.schema_version != STATE_SCHEMA_VERSION
                        || checkpoint.journal_sequence != entry.sequence
                        || state.journal_sequence > entry.sequence
                    {
                        return Err(MaintenanceError::StateUnknown(
                            "maintenance journal checkpoint is inconsistent".to_string(),
                        ));
                    }
                    state = (**checkpoint).clone();
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
        let mut file = OpenOptions::new().append(true).open(&journal_path)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    if state.journal_sequence > last_sequence {
        return Err(MaintenanceError::StateUnknown(
            "maintenance journal is truncated before the durable snapshot".to_string(),
        ));
    }
    if state.journal_sequence > snapshot_sequence {
        write_snapshot_atomic(directory, &state)?;
    }
    Ok(state)
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

fn set_shared_directory(path: &Path) -> Result<(), MaintenanceError> {
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
        } else if cfg!(test) && owner == nix::unistd::geteuid().as_raw() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o2770))?;
        } else {
            return Err(MaintenanceError::StateUnknown(
                "shared state directory must be root-owned and mode 2770".to_string(),
            ));
        }
    }
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
            schema_version: STATE_SCHEMA_VERSION,
            generation: 7,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-catalogs".to_string(),
                uid: 1001,
                gid: Some(1000),
                source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
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

    fn bootstrap_setup() -> (TempDir, RuntimeMaintenance, ReadinessRequest) {
        let dir = TempDir::new().unwrap();
        let catalog = TrustedActivitySourceCatalog {
            schema_version: STATE_SCHEMA_VERSION,
            generation: 7,
            sources: vec![TrustedActivitySource {
                source_id: "cyrene-catalogs".to_string(),
                uid: 1001,
                gid: Some(1000),
                source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
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
            let reopened =
                RuntimeMaintenance::open(dir.path(), gate.inner.catalog.read().unwrap().clone())
                    .unwrap();
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
                schema_version: STATE_SCHEMA_VERSION,
                generation: 7,
                sources: vec![TrustedActivitySource {
                    source_id: "cyrene-catalogs".to_string(),
                    uid: 1001,
                    gid: Some(1000),
                    source_token_sha256: format!("{:x}", Sha256::digest(b"test-source-token")),
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
}
