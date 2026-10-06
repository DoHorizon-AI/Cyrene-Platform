//! Cross-process Product dispatch admission for the native Control Host.
//!
//! The root-owned adoption helper is the only writer of the deployment receipt. This module
//! reads that receipt under the helper's shared lock and keeps the lock through the entire
//! asynchronous Product dispatch, so closing admission cannot race an in-flight command.
//!
//! Native Control Host 的跨进程 Product 分发准入。root helper 写入持久 receipt；本模块只读，
//! 并在整个异步分发期间持有共享锁，避免关闭准入与新命令竞争。

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Environment setting that opts a service into the first-adoption Control Host gate.
pub const CONTROL_HOST_ADMISSION_PROFILE_ENV: &str = "CYRENE_CONTROL_HOST_ADMISSION_PROFILE";
/// The only Control Host profile supported by this admission protocol.
pub const CONTROL_HOST_ADMISSION_PROFILE: &str = "cyrene.control-host.v1";
/// Stable identity reported by the live control-plane gate in health evidence.
pub const CONTROL_HOST_ADMISSION_SOURCE: &str =
    "cy-workspace-control-plane.deployment-admission.v1";
/// Stable Product-neutral scope closed by this gate.
pub const CONTROL_HOST_ADMISSION_SCOPE: &str = "workspace-product-v2/control-host";
/// Strict receipt schema written by the privileged native adoption helper.
pub const CONTROL_HOST_ADMISSION_SCHEMA: &str = "cyrene.control-host.product-admission.v1";
/// Root-owned persistent receipt directory used by production Control Hosts.
pub const CONTROL_HOST_ADMISSION_DIRECTORY: &str =
    "/var/lib/cyrene-control-host/deployment-admission";

const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = "admission.lock";
const MAX_RECEIPT_BYTES: u64 = 32 * 1024;

/// Stable errors returned when a Product dispatch cannot prove current admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeploymentAdmissionError {
    /// The deployment receipt explicitly keeps Product admission closed.
    #[error("CONTROL_HOST_PRODUCT_ADMISSION_CLOSED")]
    Closed,
    /// The receipt, profile, file permissions, or cross-process guard is unavailable or invalid.
    #[error("CONTROL_HOST_PRODUCT_ADMISSION_UNAVAILABLE")]
    Unavailable,
}

/// Current non-secret evidence exposed to the Control Host health endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionHealthEvidence {
    /// Whether Control Host identity and health APIs can continue serving.
    pub api_ready: bool,
    /// Whether Product commands may be admitted at this instant.
    pub execution_ready: bool,
    /// Product readiness; held deployments deliberately remain false.
    pub business_ready: bool,
    /// Source, scope, and receipt proof consumed by the root adoption helper.
    pub gate: AdmissionGateEvidence,
}

/// Gate proof reported without exposing credentials or mutable caller input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionGateEvidence {
    /// Stable implementation identity consumed by native deployment tooling.
    pub source: &'static str,
    /// Product-neutral command scope governed by this receipt.
    pub scope: &'static str,
    /// `open`, `closed`, `unknown`, or legacy `not_applicable`.
    pub state: &'static str,
    /// Effective group ID bound to this receipt and used by the service file guard.
    pub reader_gid: Option<u32>,
    /// Monotonic helper-owned receipt generation when available.
    pub generation: Option<u64>,
    /// Exact component catalog digest bound to the adoption plan.
    pub catalog_digest: Option<String>,
    /// Exact Workspace topology digest bound to the adoption plan.
    pub topology_digest: Option<String>,
    /// Exact adoption plan digest bound to the held or active receipt.
    pub plan_digest: Option<String>,
    /// Non-secret identity and lifecycle fields copied from the helper receipt.
    pub adoption_hold: Option<AdoptionHoldEvidence>,
}

/// Non-secret adoption identity and phase bound to the active signed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptionHoldEvidence {
    /// Root helper's idempotent adoption transaction identity.
    pub request_id: String,
    /// Organization identity copied from the signed adoption plan.
    pub organization_id: String,
    /// Workspace identity copied from the signed adoption plan.
    pub workspace_id: String,
    /// Authority identity copied from the signed adoption plan.
    pub authority_instance_id: String,
    /// Explicit prepared, held, or settled phase.
    pub phase: AdoptionHoldPhase,
    /// Plan digest repeated for exact identity cross-checking.
    pub plan_digest: String,
}

/// Service-owned Workspace identity used to bind this process to its held deployment receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentIdentity {
    /// Configured organization identity from the service's trusted root-owned configuration.
    pub organization_id: String,
    /// Configured Workspace identity from the service's trusted root-owned configuration.
    pub workspace_id: String,
    /// Configured Authority instance identity from the service's trusted root-owned configuration.
    pub authority_instance_id: String,
}

impl DeploymentIdentity {
    fn is_valid(&self) -> bool {
        valid_identity_field(&self.organization_id, 256)
            && valid_identity_field(&self.workspace_id, 256)
            && valid_identity_field(&self.authority_instance_id, 256)
    }
}

/// Explicit first-adoption lifecycle reported by the privileged helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdoptionHoldPhase {
    PreparedHeld,
    ActiveHeld,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReceiptState {
    Closed,
    Open,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeploymentAdmissionReceipt {
    schema: String,
    source: String,
    scope: String,
    state: ReceiptState,
    generation: u64,
    reader_gid: u32,
    catalog_digest: String,
    topology_digest: String,
    plan_digest: String,
    adoption_hold: AdoptionHoldEvidence,
}

#[derive(Debug, Clone)]
enum AdmissionMode {
    Legacy,
    Profile { directory: PathBuf },
    Invalid,
}

/// Reader and cross-process guard for the Control Host Product dispatch receipt.
#[derive(Debug, Clone)]
pub struct DeploymentAdmission {
    mode: AdmissionMode,
    require_root_owner: bool,
    expected_identity: Option<DeploymentIdentity>,
}

/// RAII shared-lock guard held until the complete Product dispatch transaction finishes.
#[derive(Debug)]
pub struct DispatchAdmissionGuard {
    #[cfg(unix)]
    _lock_file: Option<nix::fcntl::Flock<File>>,
}

impl DeploymentAdmission {
    /// Selects legacy single-host behavior or the one supported native Control Host profile.
    ///
    /// An unknown non-empty profile is a configuration error and must prevent service startup.
    pub fn from_environment(
        identity: DeploymentIdentity,
    ) -> Result<Self, DeploymentAdmissionError> {
        match env::var(CONTROL_HOST_ADMISSION_PROFILE_ENV) {
            Err(env::VarError::NotPresent) => Ok(Self {
                mode: AdmissionMode::Legacy,
                require_root_owner: true,
                expected_identity: None,
            }),
            Err(env::VarError::NotUnicode(_)) => Err(DeploymentAdmissionError::Unavailable),
            Ok(profile) if profile == CONTROL_HOST_ADMISSION_PROFILE => {
                if !identity.is_valid() {
                    return Err(DeploymentAdmissionError::Unavailable);
                }
                #[cfg(not(target_os = "linux"))]
                return Err(DeploymentAdmissionError::Unavailable);
                #[cfg(target_os = "linux")]
                Ok(Self {
                    mode: AdmissionMode::Profile {
                        directory: PathBuf::from(CONTROL_HOST_ADMISSION_DIRECTORY),
                    },
                    require_root_owner: true,
                    expected_identity: Some(identity),
                })
            }
            Ok(_) => Err(DeploymentAdmissionError::Unavailable),
        }
    }

    /// Acquires the shared deployment lock and verifies the exact root-owned receipt.
    ///
    /// The returned guard must remain alive until the entire command-side effect has completed.
    pub async fn acquire_dispatch(
        &self,
        organization_id: &str,
        workspace_id: &str,
        authority_instance_id: &str,
    ) -> Result<DispatchAdmissionGuard, DeploymentAdmissionError> {
        let AdmissionMode::Profile { directory } = &self.mode else {
            return match &self.mode {
                AdmissionMode::Legacy => Ok(DispatchAdmissionGuard {
                    #[cfg(unix)]
                    _lock_file: None,
                }),
                AdmissionMode::Invalid => Err(DeploymentAdmissionError::Unavailable),
                AdmissionMode::Profile { .. } => unreachable!("profile mode matched above"),
            };
        };
        let expected_identity = self
            .expected_identity
            .as_ref()
            .ok_or(DeploymentAdmissionError::Unavailable)?;
        if expected_identity.organization_id != organization_id
            || expected_identity.workspace_id != workspace_id
            || expected_identity.authority_instance_id != authority_instance_id
        {
            return Err(DeploymentAdmissionError::Unavailable);
        }
        let directory = directory.clone();
        let require_root_owner = self.require_root_owner;
        let expected_identity = expected_identity.clone();
        tokio::task::spawn_blocking(move || {
            acquire_dispatch_guard(&directory, require_root_owner, &expected_identity)
        })
        .await
        .map_err(|_| DeploymentAdmissionError::Unavailable)?
    }

    /// Returns read-only health evidence. A closed gate does not make identity or health APIs unready.
    pub fn health_evidence(&self, api_ready: bool) -> AdmissionHealthEvidence {
        match &self.mode {
            AdmissionMode::Legacy => evidence(api_ready, "not_applicable", None),
            AdmissionMode::Invalid => evidence(api_ready, "unknown", None),
            AdmissionMode::Profile { directory } => {
                match read_receipt_with_shared_lock(directory, self.require_root_owner) {
                    Ok(receipt)
                        if self.expected_identity.as_ref().is_some_and(|identity| {
                            receipt_matches_identity(&receipt, identity)
                        }) =>
                    {
                        evidence_for_receipt(api_ready, &receipt)
                    }
                    Err(_) => evidence(api_ready, "unknown", None),
                    Ok(_) => evidence(api_ready, "unknown", None),
                }
            }
        }
    }

    pub(crate) fn invalid_configuration() -> Self {
        Self {
            mode: AdmissionMode::Invalid,
            require_root_owner: true,
            expected_identity: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(directory: PathBuf, expected_identity: DeploymentIdentity) -> Self {
        Self {
            mode: AdmissionMode::Profile { directory },
            require_root_owner: false,
            expected_identity: Some(expected_identity),
        }
    }
}

fn evidence(
    api_ready: bool,
    state: &'static str,
    receipt: Option<&DeploymentAdmissionReceipt>,
) -> AdmissionHealthEvidence {
    let gate = receipt.map_or_else(
        || AdmissionGateEvidence {
            source: CONTROL_HOST_ADMISSION_SOURCE,
            scope: CONTROL_HOST_ADMISSION_SCOPE,
            state,
            reader_gid: None,
            generation: None,
            catalog_digest: None,
            topology_digest: None,
            plan_digest: None,
            adoption_hold: None,
        },
        |receipt| AdmissionGateEvidence {
            source: CONTROL_HOST_ADMISSION_SOURCE,
            scope: CONTROL_HOST_ADMISSION_SCOPE,
            state,
            reader_gid: Some(receipt.reader_gid),
            generation: Some(receipt.generation),
            catalog_digest: Some(receipt.catalog_digest.clone()),
            topology_digest: Some(receipt.topology_digest.clone()),
            plan_digest: Some(receipt.plan_digest.clone()),
            adoption_hold: Some(receipt.adoption_hold.clone()),
        },
    );
    let execution_ready = api_ready && matches!(state, "open" | "not_applicable");
    AdmissionHealthEvidence {
        api_ready,
        execution_ready,
        business_ready: execution_ready,
        gate,
    }
}

fn evidence_for_receipt(
    api_ready: bool,
    receipt: &DeploymentAdmissionReceipt,
) -> AdmissionHealthEvidence {
    let state = match receipt.state {
        ReceiptState::Closed => "closed",
        ReceiptState::Open => "open",
    };
    evidence(api_ready, state, Some(receipt))
}

#[cfg(unix)]
fn acquire_dispatch_guard(
    directory: &Path,
    require_root_owner: bool,
    expected_identity: &DeploymentIdentity,
) -> Result<DispatchAdmissionGuard, DeploymentAdmissionError> {
    use nix::fcntl::{Flock, FlockArg};

    let lock_file = Flock::lock(
        open_trusted_file(directory, LOCK_FILE, require_root_owner)?,
        FlockArg::LockShared,
    )
    .map_err(|_| DeploymentAdmissionError::Unavailable)?;
    let receipt = read_locked_receipt(directory, require_root_owner)?;
    if !receipt_matches_identity(&receipt, expected_identity) {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    match receipt.state {
        ReceiptState::Open => Ok(DispatchAdmissionGuard {
            _lock_file: Some(lock_file),
        }),
        ReceiptState::Closed => Err(DeploymentAdmissionError::Closed),
    }
}

#[cfg(not(unix))]
fn acquire_dispatch_guard(
    _directory: &Path,
    _require_root_owner: bool,
    _expected_identity: &DeploymentIdentity,
) -> Result<DispatchAdmissionGuard, DeploymentAdmissionError> {
    Err(DeploymentAdmissionError::Unavailable)
}

fn read_receipt_with_shared_lock(
    directory: &Path,
    require_root_owner: bool,
) -> Result<DeploymentAdmissionReceipt, DeploymentAdmissionError> {
    #[cfg(unix)]
    {
        use nix::fcntl::{Flock, FlockArg};

        let _lock_file = Flock::lock(
            open_trusted_file(directory, LOCK_FILE, require_root_owner)?,
            FlockArg::LockShared,
        )
        .map_err(|_| DeploymentAdmissionError::Unavailable)?;
        read_locked_receipt(directory, require_root_owner)
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, require_root_owner);
        Err(DeploymentAdmissionError::Unavailable)
    }
}

fn read_locked_receipt(
    directory: &Path,
    require_root_owner: bool,
) -> Result<DeploymentAdmissionReceipt, DeploymentAdmissionError> {
    verify_directory(directory, require_root_owner)?;
    let file = open_trusted_file(directory, STATE_FILE, require_root_owner)?;
    let metadata = file
        .metadata()
        .map_err(|_| DeploymentAdmissionError::Unavailable)?;
    if metadata.len() > MAX_RECEIPT_BYTES {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DeploymentAdmissionError::Unavailable)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    let receipt: DeploymentAdmissionReceipt =
        serde_json::from_slice(&bytes).map_err(|_| DeploymentAdmissionError::Unavailable)?;
    validate_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_receipt(receipt: &DeploymentAdmissionReceipt) -> Result<(), DeploymentAdmissionError> {
    if receipt.schema != CONTROL_HOST_ADMISSION_SCHEMA
        || receipt.source != "control-host-adoption-helper.v1"
        || receipt.scope != CONTROL_HOST_ADMISSION_SCOPE
        || receipt.generation == 0
        || !valid_reader_gid(receipt.reader_gid, effective_gid()?)
        || !is_sha256(&receipt.catalog_digest)
        || !is_sha256(&receipt.topology_digest)
        || !is_sha256(&receipt.plan_digest)
        || receipt.adoption_hold.plan_digest != receipt.plan_digest
        || !valid_identity_field(&receipt.adoption_hold.request_id, 128)
        || !valid_identity_field(&receipt.adoption_hold.organization_id, 256)
        || !valid_identity_field(&receipt.adoption_hold.workspace_id, 256)
        || !valid_identity_field(&receipt.adoption_hold.authority_instance_id, 256)
    {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    let phase_matches_state = matches!(
        (receipt.state, receipt.adoption_hold.phase),
        (ReceiptState::Closed, AdoptionHoldPhase::PreparedHeld)
            | (ReceiptState::Closed, AdoptionHoldPhase::ActiveHeld)
            | (ReceiptState::Open, AdoptionHoldPhase::Active)
    );
    if !phase_matches_state {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    Ok(())
}

fn receipt_matches_identity(
    receipt: &DeploymentAdmissionReceipt,
    expected: &DeploymentIdentity,
) -> bool {
    receipt.adoption_hold.organization_id == expected.organization_id
        && receipt.adoption_hold.workspace_id == expected.workspace_id
        && receipt.adoption_hold.authority_instance_id == expected.authority_instance_id
}

fn is_sha256(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_identity_field(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_reader_gid(recorded: u32, effective: u32) -> bool {
    recorded == effective
}

#[cfg(unix)]
fn effective_gid() -> Result<u32, DeploymentAdmissionError> {
    Ok(nix::unistd::getegid().as_raw())
}

#[cfg(not(unix))]
fn effective_gid() -> Result<u32, DeploymentAdmissionError> {
    Err(DeploymentAdmissionError::Unavailable)
}

#[cfg(unix)]
fn open_trusted_file(
    directory: &Path,
    name: &str,
    require_root_owner: bool,
) -> Result<File, DeploymentAdmissionError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    verify_directory(directory, require_root_owner)?;
    let path = directory.join(name);
    let before = fs::symlink_metadata(&path).map_err(|_| DeploymentAdmissionError::Unavailable)?;
    if !before.file_type().is_file()
        || (require_root_owner && before.uid() != 0)
        || before.gid() != effective_gid()?
        || before.permissions().mode() & 0o777 != expected_file_mode(name)
    {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(name == LOCK_FILE)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| DeploymentAdmissionError::Unavailable)?;
    let after = file
        .metadata()
        .map_err(|_| DeploymentAdmissionError::Unavailable)?;
    if !after.file_type().is_file()
        || (require_root_owner && after.uid() != 0)
        || after.gid() != effective_gid()?
        || after.permissions().mode() & 0o777 != expected_file_mode(name)
    {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    Ok(file)
}

#[cfg(unix)]
fn expected_file_mode(name: &str) -> u32 {
    if name == LOCK_FILE {
        0o660
    } else {
        0o640
    }
}

#[cfg(not(unix))]
fn open_trusted_file(
    _directory: &Path,
    _name: &str,
    _require_root_owner: bool,
) -> Result<File, DeploymentAdmissionError> {
    Err(DeploymentAdmissionError::Unavailable)
}

#[cfg(unix)]
fn verify_directory(
    directory: &Path,
    require_root_owner: bool,
) -> Result<(), DeploymentAdmissionError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata =
        fs::symlink_metadata(directory).map_err(|_| DeploymentAdmissionError::Unavailable)?;
    if !metadata.file_type().is_dir()
        || (require_root_owner && metadata.uid() != 0)
        || metadata.permissions().mode() & 0o777 != 0o750
        || metadata.gid() != effective_gid()?
    {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    if require_root_owner {
        verify_fixed_parent_directories(directory)?;
    }
    Ok(())
}

#[cfg(unix)]
fn verify_fixed_parent_directories(directory: &Path) -> Result<(), DeploymentAdmissionError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if directory != Path::new(CONTROL_HOST_ADMISSION_DIRECTORY) {
        return Err(DeploymentAdmissionError::Unavailable);
    }
    for parent in [
        Path::new("/var"),
        Path::new("/var/lib"),
        Path::new("/var/lib/cyrene-control-host"),
    ] {
        let metadata =
            fs::symlink_metadata(parent).map_err(|_| DeploymentAdmissionError::Unavailable)?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(DeploymentAdmissionError::Unavailable);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_directory(
    _directory: &Path,
    _require_root_owner: bool,
) -> Result<(), DeploymentAdmissionError> {
    Err(DeploymentAdmissionError::Unavailable)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn identity() -> DeploymentIdentity {
        DeploymentIdentity {
            organization_id: "organization-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            authority_instance_id: "authority-1".to_owned(),
        }
    }

    fn install_receipt(directory: &Path, state: &str, phase: &str) {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(directory.join(LOCK_FILE), b"").unwrap();
        fs::set_permissions(directory.join(LOCK_FILE), fs::Permissions::from_mode(0o660)).unwrap();
        let receipt = json!({
            "schema": CONTROL_HOST_ADMISSION_SCHEMA,
            "source": "control-host-adoption-helper.v1",
            "scope": CONTROL_HOST_ADMISSION_SCOPE,
            "state": state,
            "generation": 4,
            "readerGid": nix::unistd::getegid().as_raw(),
            "catalogDigest": format!("sha256:{}", "a".repeat(64)),
            "topologyDigest": format!("sha256:{}", "b".repeat(64)),
            "planDigest": format!("sha256:{}", "c".repeat(64)),
            "adoptionHold": {
                "requestId": "adoption-1",
                "organizationId": "organization-1",
                "workspaceId": "workspace-1",
                "authorityInstanceId": "authority-1",
                "phase": phase,
                "planDigest": format!("sha256:{}", "c".repeat(64))
            }
        });
        fs::write(
            directory.join(STATE_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            directory.join(STATE_FILE),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn closed_and_missing_receipts_fail_closed_but_keep_api_ready() {
        let temp = tempdir().unwrap();
        let missing = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        assert_eq!(
            missing
                .acquire_dispatch("organization-1", "workspace-1", "authority-1")
                .await
                .unwrap_err(),
            DeploymentAdmissionError::Unavailable
        );

        install_receipt(temp.path(), "closed", "ACTIVE_HELD");
        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        assert_eq!(
            admission
                .acquire_dispatch("organization-1", "workspace-1", "authority-1")
                .await
                .unwrap_err(),
            DeploymentAdmissionError::Closed
        );
        let health = admission.health_evidence(true);
        assert!(health.api_ready);
        assert!(!health.execution_ready);
        assert!(!health.business_ready);
        assert_eq!(health.gate.state, "closed");
        assert_eq!(health.gate.source, CONTROL_HOST_ADMISSION_SOURCE);
        assert_eq!(health.gate.scope, CONTROL_HOST_ADMISSION_SCOPE);
        assert_eq!(
            health.gate.plan_digest.as_deref(),
            Some(format!("sha256:{}", "c".repeat(64)).as_str())
        );
    }

    #[tokio::test]
    async fn open_receipt_holds_shared_lock_across_processes_until_dispatch_guard_drops() {
        use std::process::Command;

        let temp = tempdir().unwrap();
        install_receipt(temp.path(), "open", "ACTIVE");
        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        let guard = admission
            .acquire_dispatch("organization-1", "workspace-1", "authority-1")
            .await
            .unwrap();
        assert!(admission.health_evidence(true).execution_ready);
        assert_eq!(
            admission.health_evidence(true).gate.reader_gid,
            Some(effective_gid().unwrap())
        );

        let probe = |expected: &str| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "deployment_admission::tests::lock_probe_child",
                    "--nocapture",
                ])
                .env(
                    "CYRENE_ADMISSION_LOCK_PROBE_PATH",
                    temp.path().join(LOCK_FILE),
                )
                .env("CYRENE_ADMISSION_LOCK_PROBE_EXPECTED", expected)
                .status()
                .unwrap()
        };
        assert!(probe("blocked").success());
        drop(guard);
        assert!(probe("acquired").success());
    }

    #[test]
    fn lock_probe_child() {
        use nix::fcntl::{Flock, FlockArg};

        let Ok(path) = std::env::var("CYRENE_ADMISSION_LOCK_PROBE_PATH") else {
            return;
        };
        let expected = std::env::var("CYRENE_ADMISSION_LOCK_PROBE_EXPECTED").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let result = Flock::lock(file, FlockArg::LockExclusiveNonblock);
        match expected.as_str() {
            "blocked" => assert!(result.is_err()),
            "acquired" => assert!(result.is_ok()),
            _ => panic!("invalid lock probe expectation"),
        }
    }

    #[test]
    fn production_admission_source_and_path_are_fixed() {
        assert_eq!(
            CONTROL_HOST_ADMISSION_DIRECTORY,
            "/var/lib/cyrene-control-host/deployment-admission"
        );
        assert_eq!(
            CONTROL_HOST_ADMISSION_SOURCE,
            "cy-workspace-control-plane.deployment-admission.v1"
        );
    }

    #[test]
    fn malformed_or_mismatched_receipts_report_unknown() {
        let temp = tempdir().unwrap();
        install_receipt(temp.path(), "closed", "ACTIVE_HELD");
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(STATE_FILE)).unwrap()).unwrap();
        receipt["planDigest"] = json!("d".repeat(64));
        fs::write(
            temp.path().join(STATE_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        let health = admission.health_evidence(true);
        assert!(health.api_ready);
        assert!(!health.execution_ready);
        assert_eq!(health.gate.state, "unknown");
        assert_eq!(
            tokio_test_acquire(&admission),
            DeploymentAdmissionError::Unavailable
        );
    }

    #[test]
    fn unknown_receipt_metadata_is_rejected_instead_of_becoming_authoritative() {
        let temp = tempdir().unwrap();
        install_receipt(temp.path(), "closed", "ACTIVE_HELD");
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(STATE_FILE)).unwrap()).unwrap();
        receipt["cloudInstanceName"] = json!("attacker-controlled");
        fs::write(
            temp.path().join(STATE_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();

        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        assert_eq!(admission.health_evidence(true).gate.state, "unknown");
        assert_eq!(
            tokio_test_acquire(&admission),
            DeploymentAdmissionError::Unavailable
        );
    }

    #[test]
    fn receipt_reader_gid_tracks_native_and_container_service_identities() {
        assert!(valid_reader_gid(999, 999));
        assert!(valid_reader_gid(10_001, 10_001));
        assert!(!valid_reader_gid(999, 10_001));
        assert!(!valid_reader_gid(10_001, 999));
    }

    #[tokio::test]
    async fn receipt_and_request_identity_must_match_the_service_profile() {
        let temp = tempdir().unwrap();
        install_receipt(temp.path(), "open", "ACTIVE");
        let wrong_identity = DeploymentIdentity {
            organization_id: "another-organization".to_owned(),
            ..identity()
        };
        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), wrong_identity);
        assert_eq!(admission.health_evidence(true).gate.state, "unknown");
        assert_eq!(
            admission
                .acquire_dispatch("another-organization", "workspace-1", "authority-1")
                .await
                .unwrap_err(),
            DeploymentAdmissionError::Unavailable
        );

        let admission = DeploymentAdmission::for_test(temp.path().to_path_buf(), identity());
        assert_eq!(
            admission
                .acquire_dispatch("organization-1", "another-workspace", "authority-1")
                .await
                .unwrap_err(),
            DeploymentAdmissionError::Unavailable
        );
    }

    #[test]
    fn sha256_digest_requires_one_canonical_prefix() {
        assert!(is_sha256(&format!("sha256:{}", "a".repeat(64))));
        assert!(!is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&format!("sha256:{}", "A".repeat(64))));
        assert!(!is_sha256(&format!("sha256:{}", "a".repeat(63))));
    }

    #[test]
    fn legacy_without_profile_preserves_existing_open_behavior() {
        let admission = DeploymentAdmission {
            mode: AdmissionMode::Legacy,
            require_root_owner: true,
            expected_identity: None,
        };
        let health = admission.health_evidence(true);
        assert!(health.api_ready);
        assert!(health.execution_ready);
        assert!(health.business_ready);
        assert_eq!(health.gate.state, "not_applicable");
    }

    fn tokio_test_acquire(admission: &DeploymentAdmission) -> DeploymentAdmissionError {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(admission.acquire_dispatch("organization-1", "workspace-1", "authority-1"));
        result.unwrap_err()
    }
}
