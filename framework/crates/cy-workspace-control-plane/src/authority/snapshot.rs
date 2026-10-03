//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Product contract snapshot activation and durable fencing          │
//! │  Module: cy_workspace_control_plane::authority::snapshot           │
//! │  Role: Keep the active catalog/policy pair immutable and monotonic. │
//! │                                                                     │
//! │  模块职责：原子激活目录与策略快照，并将 generation/epoch 持久化。    │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Return the current wall-clock time as Unix milliseconds.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Stable identity of one immutable bundle/policy pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotIdentity {
    /// Product wire protocol selected by the released catalogs.
    pub wire_api_version: String,
    /// Exact SHA-256 of the Product bundle manifest bytes.
    pub bundle_manifest_sha256: String,
    /// Exact SHA-256 of the independently approved policy bytes.
    pub policy_sha256: String,
    /// SHA-256 of the immutable staged archive that contains this pair.
    pub content_digest: String,
    /// Platform source commit that owns the independently approved policy.
    pub policy_source_commit: String,
    /// Product owner source commits, keyed by stable owner ID.
    pub source_commits: std::collections::BTreeMap<String, String>,
    /// SHA-256 of each owner's catalog file, keyed by stable owner ID.
    pub owner_catalog_digests: std::collections::BTreeMap<String, String>,
}

/// Immutable catalog and separately approved authorization policy.
#[derive(Clone)]
pub struct ContractSnapshot {
    /// Monotonic generation used by final Authority fences.
    pub generation: u64,
    /// Monotonic activation epoch; currently advances with generation.
    pub activation_epoch: u64,
    /// Verified Product catalog data.
    pub bundle: Arc<ProductContractBundle>,
    /// Independently verified Platform authorization policy.
    pub policy: Arc<TrustedProductPolicy>,
    /// Runtime pins derived from the trusted release proof.
    pub pins: ProductBundlePins,
    /// Identity of the immutable pair and its source commits.
    pub identity: SnapshotIdentity,
    /// ID of the immutable staged artifact directory.
    pub artifact_id: String,
    /// Activation time recorded for observability.
    pub activated_at_unix_ms: u64,
}

/// Error returned when a requested snapshot cannot safely become active.
#[derive(Debug, Error)]
pub enum SnapshotActivationError {
    /// Requested generation is stale or reused.
    #[error("generation must exceed the durable high-water mark ({current} >= {attempted})")]
    GenerationNotMonotonic { current: u64, attempted: u64 },
    /// Snapshot pair or proof is malformed.
    #[error("snapshot validation failed: {0}")]
    SnapshotInvalid(String),
    /// Durable activation state could not be read or written safely.
    #[error("snapshot activation state is unavailable: {0}")]
    StateIo(#[from] io::Error),
    /// Existing persisted state does not match the verified active artifact.
    #[error("verified snapshot does not match the persisted active pointer")]
    ActivePointerMismatch,
    /// Persisted activation record is malformed or unsupported.
    #[error("persisted activation record is invalid")]
    ActivationRecordInvalid,
    /// Lock was poisoned after a prior panic; fail closed instead of serving stale state.
    #[error("snapshot state lock is poisoned")]
    LockPoisoned,
}

/// Durable pointer to the active artifact, stored outside versioned data directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotActivationRecord {
    /// Record format version.
    pub format_version: u32,
    /// Highest generation ever committed, including rollbacks.
    pub highest_generation: u64,
    /// Generation currently serving requests.
    pub current_generation: u64,
    /// Current Authority fence epoch.
    pub activation_epoch: u64,
    /// Immutable artifact directory key, never an arbitrary filesystem path.
    pub artifact_id: String,
    /// Locally confirmed maintenance transaction that requested activation.
    pub plan_id: String,
    /// Digest of the exact user-confirmed maintenance plan.
    pub plan_digest: String,
    /// Catalog/policy/source identity of the current pair.
    pub identity: SnapshotIdentity,
}

/// A snapshot that passed `SnapshotTrust` verification.
///
/// Fields are private so production activation cannot accept a raw request or caller-built pair.
pub struct VerifiedContractSnapshot {
    pub(super) snapshot: ContractSnapshot,
}

impl VerifiedContractSnapshot {
    /// Build a token only inside the control-plane trust verifier.
    pub(super) fn from_verified(snapshot: ContractSnapshot) -> Self {
        Self { snapshot }
    }

    /// Return the exact identity that was verified before activation.
    pub fn identity(&self) -> &SnapshotIdentity {
        &self.snapshot.identity
    }

    /// Return the generation requested by the local activation plan.
    pub fn generation(&self) -> u64 {
        self.snapshot.generation
    }
}

/// Thread-safe snapshot manager with an atomically persisted current pointer.
pub struct ContractSnapshotManager {
    state_dir: PathBuf,
    current: RwLock<Arc<ContractSnapshot>>,
    highest_generation: AtomicU64,
}

/// Name used by hosts that explicitly open the persistent manager.
pub type PersistentContractSnapshotManager = ContractSnapshotManager;

impl ContractSnapshotManager {
    /// Open durable state and recover only the artifact selected by its protected pointer.
    ///
    /// If no pointer exists, the verified initial snapshot is committed as generation one.
    /// Existing malformed state or a mismatch fails closed; it never falls back to a new bundle.
    pub fn open(
        state_dir: impl AsRef<Path>,
        initial: VerifiedContractSnapshot,
    ) -> Result<Self, SnapshotActivationError> {
        let (state_dir, created_state_dir) = prepare_state_dir(state_dir.as_ref())?;
        let state_path = activation_record_path(&state_dir);
        let snapshot = initial.snapshot;
        validate_snapshot_identity(&snapshot)?;

        let record = match fs::symlink_metadata(&state_path) {
            Ok(_) => {
                let record = read_activation_record(&state_path)?;
                if record.current_generation != snapshot.generation
                    || record.activation_epoch != snapshot.activation_epoch
                    || record.artifact_id != snapshot.artifact_id
                    || record.identity != snapshot.identity
                    || record.highest_generation < record.current_generation
                {
                    return Err(SnapshotActivationError::ActivePointerMismatch);
                }
                record
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Only an Authority's first boot may create the state directory and bootstrap
                // generation one. An existing directory without its durable pointer indicates
                // lost or damaged high-water state and must never reset generations.
                if !created_state_dir || snapshot.generation != 1 || snapshot.activation_epoch != 1
                {
                    return Err(SnapshotActivationError::ActivePointerMismatch);
                }
                let record =
                    record_for(&snapshot, 1, "bootstrap", &snapshot.identity.content_digest);
                persist_activation_record(&state_dir, &record)?;
                record
            }
            Err(error) => return Err(SnapshotActivationError::StateIo(error)),
        };

        Ok(Self {
            state_dir,
            current: RwLock::new(Arc::new(snapshot)),
            highest_generation: AtomicU64::new(record.highest_generation),
        })
    }

    /// Read the durable pointer before loading a snapshot after process restart.
    pub fn read_activation_record(
        state_dir: impl AsRef<Path>,
    ) -> Result<Option<SnapshotActivationRecord>, SnapshotActivationError> {
        let state_dir = state_dir.as_ref();
        match fs::symlink_metadata(state_dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SnapshotActivationError::StateIo(error)),
            Ok(_) => {}
        }
        validate_state_dir(state_dir)?;
        let path = activation_record_path(state_dir);
        match fs::symlink_metadata(&path) {
            Ok(_) => read_activation_record(&path).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SnapshotActivationError::StateIo(error)),
        }
    }

    /// Read the immutable current snapshot used by all Authority decisions.
    pub fn active_snapshot(&self) -> Result<Arc<ContractSnapshot>, SnapshotActivationError> {
        self.current
            .read()
            .map(|current| Arc::clone(&current))
            .map_err(|_| SnapshotActivationError::LockPoisoned)
    }

    /// Return the protected directory holding the durable activation record.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Atomically advance the durable generation and switch to a trust-verified pair.
    ///
    /// `requested_generation` comes from the locally confirmed activation plan. The manager
    /// checks it against the durable high-water mark before committing the pointer.
    pub fn activate_verified(
        &self,
        verified: VerifiedContractSnapshot,
        plan_id: &str,
        plan_digest: &str,
        expected_generation: u64,
    ) -> Result<SnapshotActivationRecord, SnapshotActivationError> {
        let snapshot = verified.snapshot;
        validate_snapshot_identity(&snapshot)?;
        if !valid_plan_id(plan_id) || !is_sha256_prefixed_digest(plan_digest) {
            return Err(SnapshotActivationError::SnapshotInvalid(
                "confirmed activation plan identity is invalid".to_owned(),
            ));
        }
        let mut current = self
            .current
            .write()
            .map_err(|_| SnapshotActivationError::LockPoisoned)?;
        let high = self.highest_generation.load(Ordering::SeqCst);
        if expected_generation != high
            || snapshot.generation != expected_generation.saturating_add(1)
            || snapshot.activation_epoch != snapshot.generation
        {
            return Err(SnapshotActivationError::GenerationNotMonotonic {
                current: expected_generation,
                attempted: snapshot.generation,
            });
        }

        let record = record_for(&snapshot, snapshot.generation, plan_id, plan_digest);
        persist_activation_record(&self.state_dir, &record)?;
        self.highest_generation
            .store(snapshot.generation, Ordering::SeqCst);
        *current = Arc::new(snapshot);
        Ok(record)
    }

    /// Return the durable generation high-water mark used for the next local plan.
    pub fn highest_generation(&self) -> u64 {
        self.highest_generation.load(Ordering::SeqCst)
    }
}

fn record_for(
    snapshot: &ContractSnapshot,
    high: u64,
    plan_id: &str,
    plan_digest: &str,
) -> SnapshotActivationRecord {
    SnapshotActivationRecord {
        format_version: 1,
        highest_generation: high,
        current_generation: snapshot.generation,
        activation_epoch: snapshot.activation_epoch,
        artifact_id: snapshot.artifact_id.clone(),
        plan_id: plan_id.to_owned(),
        plan_digest: plan_digest.to_owned(),
        identity: snapshot.identity.clone(),
    }
}

fn validate_snapshot_identity(snapshot: &ContractSnapshot) -> Result<(), SnapshotActivationError> {
    if snapshot.generation == 0
        || snapshot.activation_epoch == 0
        || snapshot.artifact_id.is_empty()
        || !is_sha256_hex(&snapshot.identity.bundle_manifest_sha256)
        || !is_sha256_hex(&snapshot.identity.policy_sha256)
        || !is_sha256_prefixed_digest(&snapshot.identity.content_digest)
        || snapshot.artifact_id != snapshot.identity.content_digest
        || snapshot.identity.source_commits.is_empty()
        || snapshot.identity.owner_catalog_digests.len() != snapshot.identity.source_commits.len()
        || !snapshot
            .identity
            .owner_catalog_digests
            .keys()
            .eq(snapshot.identity.source_commits.keys())
        || !is_git_commit(&snapshot.identity.policy_source_commit)
        || snapshot
            .identity
            .source_commits
            .values()
            .any(|commit| !is_git_commit(commit))
        || snapshot.pins.manifest_sha256() != snapshot.identity.bundle_manifest_sha256
        || snapshot.pins.policy_sha256() != snapshot.identity.policy_sha256
        || snapshot.pins.wire_api_version() != snapshot.identity.wire_api_version
        || snapshot.pins.owner_source_shas() != &snapshot.identity.source_commits
        || snapshot
            .identity
            .owner_catalog_digests
            .values()
            .any(|digest| !is_sha256_hex(digest))
    {
        return Err(SnapshotActivationError::SnapshotInvalid(
            "paired snapshot identity or trust pins do not match".to_owned(),
        ));
    }
    snapshot
        .policy
        .validate_bundle(&snapshot.bundle)
        .map_err(|error| SnapshotActivationError::SnapshotInvalid(error.to_string()))
}

fn read_activation_record(
    path: &Path,
) -> Result<SnapshotActivationRecord, SnapshotActivationError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(SnapshotActivationError::ActivationRecordInvalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(SnapshotActivationError::ActivationRecordInvalid);
        }
    }
    let bytes = fs::read(path)?;
    let record: SnapshotActivationRecord = serde_json::from_slice(&bytes)
        .map_err(|_| SnapshotActivationError::ActivationRecordInvalid)?;
    if record.format_version != 1
        || record.current_generation == 0
        || record.activation_epoch == 0
        || record.activation_epoch != record.current_generation
        || record.highest_generation < record.current_generation
        || !is_sha256_prefixed_digest(&record.artifact_id)
        || record.artifact_id != record.identity.content_digest
        || !is_sha256_prefixed_digest(&record.identity.content_digest)
        || record.artifact_id.contains('/')
        || record.artifact_id.contains('\\')
        || !valid_plan_id(&record.plan_id)
        || !is_sha256_prefixed_digest(&record.plan_digest)
        || !is_sha256_hex(&record.identity.bundle_manifest_sha256)
        || !is_sha256_hex(&record.identity.policy_sha256)
        || record.identity.source_commits.is_empty()
        || record.identity.owner_catalog_digests.len() != record.identity.source_commits.len()
        || !record
            .identity
            .owner_catalog_digests
            .keys()
            .eq(record.identity.source_commits.keys())
        || !is_git_commit(&record.identity.policy_source_commit)
        || record
            .identity
            .source_commits
            .values()
            .any(|commit| !is_git_commit(commit))
        || record
            .identity
            .owner_catalog_digests
            .values()
            .any(|digest| !is_sha256_hex(digest))
    {
        return Err(SnapshotActivationError::ActivationRecordInvalid);
    }
    Ok(record)
}

fn prepare_state_dir(path: &Path) -> Result<(PathBuf, bool), io::Error> {
    let created = match fs::symlink_metadata(path) {
        Ok(_) => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe state directory",
                ));
            }
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
            true
        }
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "insecure state directory",
            ));
        }
    }
    Ok((path.to_path_buf(), created))
}

fn validate_state_dir(path: &Path) -> Result<(), SnapshotActivationError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SnapshotActivationError::ActivationRecordInvalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(SnapshotActivationError::ActivationRecordInvalid);
        }
    }
    Ok(())
}

fn activation_record_path(state_dir: &Path) -> PathBuf {
    state_dir.join("active-product-snapshot-v1.json")
}

fn persist_activation_record(
    state_dir: &Path,
    record: &SnapshotActivationRecord,
) -> Result<(), io::Error> {
    let final_path = activation_record_path(state_dir);
    let temp_path = state_dir.join(format!(
        ".activation-{}-{}-{}.tmp",
        record.current_generation,
        std::process::id(),
        now_unix_ms()
    ));
    let payload = serde_json::to_vec(record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let write_result = (|| {
        file.write_all(&payload)?;
        file.sync_all()?;
        fs::rename(&temp_path, &final_path)?;
        File::open(state_dir)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_sha256_prefixed_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256_hex)
}

fn valid_plan_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    const SOURCE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OWNER_ID: &str = "workspace";

    fn verified_snapshot(generation: u64, content_digest: &str) -> VerifiedContractSnapshot {
        let source_commits = BTreeMap::from([(OWNER_ID.to_owned(), SOURCE_COMMIT.to_owned())]);
        let owner_catalog_digests = BTreeMap::from([(OWNER_ID.to_owned(), "a".repeat(64))]);
        let bundle_manifest_sha256 = "b".repeat(64);
        let policy_sha256 = "c".repeat(64);
        let pins = ProductBundlePins::new(
            "cyrene.workspace.product.v2",
            bundle_manifest_sha256.clone(),
            source_commits.clone(),
            "cyrene.workspace.product.authorization-policy.v2",
            policy_sha256.clone(),
        );
        let snapshot = ContractSnapshot {
            generation,
            activation_epoch: generation,
            bundle: Arc::new(ProductContractBundle::empty_for_test()),
            policy: Arc::new(TrustedProductPolicy::empty_for_test()),
            pins,
            identity: SnapshotIdentity {
                wire_api_version: "cyrene.workspace.product.v2".to_owned(),
                bundle_manifest_sha256,
                policy_sha256,
                content_digest: content_digest.to_owned(),
                policy_source_commit: SOURCE_COMMIT.to_owned(),
                source_commits,
                owner_catalog_digests,
            },
            artifact_id: content_digest.to_owned(),
            activated_at_unix_ms: now_unix_ms(),
        };
        VerifiedContractSnapshot::from_verified(snapshot)
    }

    fn content_digest(byte: u8) -> String {
        format!("sha256:{}", format!("{byte:02x}").repeat(32))
    }

    #[test]
    fn activation_is_durable_monotonic_and_supports_higher_generation_rollback() {
        let temp = TempDir::new().expect("temp directory");
        let state_dir = temp.path().join("authority-state");
        let artifact_a = content_digest(0x11);
        let artifact_b = content_digest(0x22);

        let manager = ContractSnapshotManager::open(&state_dir, verified_snapshot(1, &artifact_a))
            .expect("bootstrap verified snapshot");
        let activated = manager
            .activate_verified(
                verified_snapshot(2, &artifact_b),
                "plan-2",
                &content_digest(0x33),
                1,
            )
            .expect("activate second artifact");
        assert_eq!(activated.current_generation, 2);
        assert_eq!(activated.highest_generation, 2);
        assert_eq!(manager.active_snapshot().unwrap().artifact_id, artifact_b);
        drop(manager);

        let recovered =
            ContractSnapshotManager::open(&state_dir, verified_snapshot(2, &content_digest(0x22)))
                .expect("recover persisted current artifact");
        assert_eq!(recovered.highest_generation(), 2);
        let rolled_back = recovered
            .activate_verified(
                verified_snapshot(3, &artifact_a),
                "rollback-3",
                &content_digest(0x44),
                2,
            )
            .expect("rollback by activating the old artifact at a higher generation");
        assert_eq!(rolled_back.current_generation, 3);
        assert_eq!(rolled_back.highest_generation, 3);
        assert_eq!(recovered.active_snapshot().unwrap().artifact_id, artifact_a);
        assert!(recovered
            .activate_verified(
                verified_snapshot(3, &artifact_b),
                "stale-plan",
                &content_digest(0x55),
                2,
            )
            .is_err());
    }

    #[test]
    fn corrupted_durable_record_fails_closed() {
        let temp = TempDir::new().expect("temp directory");
        let state_dir = temp.path().join("authority-state");
        let manager =
            ContractSnapshotManager::open(&state_dir, verified_snapshot(1, &content_digest(0x11)))
                .expect("bootstrap verified snapshot");
        drop(manager);
        fs::write(
            activation_record_path(&state_dir),
            br#"{"formatVersion":1,"currentGeneration":1"#,
        )
        .expect("corrupt record");
        assert!(matches!(
            ContractSnapshotManager::open(&state_dir, verified_snapshot(1, &content_digest(0x11)),),
            Err(SnapshotActivationError::ActivationRecordInvalid)
        ));
    }

    #[test]
    fn existing_state_directory_without_pointer_cannot_reset_generation() {
        let temp = TempDir::new().expect("temp directory");
        let state_dir = temp.path().join("authority-state");
        fs::create_dir(&state_dir).expect("preexisting state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))
                .expect("protect state directory");
        }
        assert!(matches!(
            ContractSnapshotManager::open(&state_dir, verified_snapshot(1, &content_digest(0x11)),),
            Err(SnapshotActivationError::ActivePointerMismatch)
        ));
    }

    #[test]
    fn missing_state_directory_allows_only_verified_first_bootstrap() {
        let temp = TempDir::new().expect("temp directory");
        let state_dir = temp.path().join("authority-state");
        assert_eq!(
            ContractSnapshotManager::read_activation_record(&state_dir)
                .expect("missing state means first boot"),
            None
        );
        let manager =
            ContractSnapshotManager::open(&state_dir, verified_snapshot(1, &content_digest(0x11)))
                .expect("first verified artifact bootstraps state");
        assert_eq!(manager.highest_generation(), 1);
    }
}
