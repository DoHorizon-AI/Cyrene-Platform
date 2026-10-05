//! Root-validated one-shot offline package installation.
//!
//! This module validates a persistent PACKAGE_ONLY maintenance hold before
//! handing a digest-pinned candidate to the dropped-privilege installer worker.
//! Workspace remains the authority for external release attestations.
//! 中文：先验证root维护hold，再以运行服务账号安装离线候选包。

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::{AsRawFd, FromRawFd, RawFd},
        net::UnixStream,
    },
    path::{Component, Path, PathBuf},
    time::Duration,
};

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    ArtifactDigest, InstallationRecord, OfflineInstallCandidateIdentity, PackageId,
    PackageRuntimeError, PackageSource, PackageVersion,
};

const MAX_BOOTSTRAP_INPUT_BYTES: usize = 64 * 1024;
const MAX_BROKER_FRAME_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_MAINTENANCE_TOKEN_BYTES: usize = 256;
const MAX_OPERATOR_TOKEN_BYTES: usize = 4096;
const MAX_CANDIDATE_DESCRIPTOR_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CANDIDATE_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const BROKER_PROTOCOL_VERSION: &str = "cyrene.runtime-maintenance.broker.v1";
const BROKER_SOCKET: &str = "/run/cyrene/runtime-maintenance.sock";
const OPERATOR_TOKEN_FILE: &str = "/var/lib/cyrene/runtime-maintenance-private/operator.token";
const PACKAGE_RUNTIME_SOCKET: &str = "/run/cyrene-package-runtime/control.sock";
const BOOTSTRAP_STAGE_ROOT: &str = "/run/cyrene-package-runtime-bootstrap";
const PERSISTENT_BOOTSTRAP_ROOT: &str = "/var/lib/cyrene-updates/plugin-package-bootstrap";
const PACKAGE_RUNTIME_ROOT: &str = "/var/lib/cyrene/package-runtime";
const DEPENDENCY_PREPARER: &str = "/usr/libexec/cyrene-plugin-python-preparer";
const PINNED_UV: &str = "/opt/cyrene/uv/0.12.21/uv";
const PINNED_PYTHON: &str = "/opt/cyrene/python/3.12.14/bin/python3.12";

/// Strict one-shot request. Hold secrets are accepted only through bounded stdin
/// or a root-owned private file, never through argv or environment variables.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapInstallInput {
    pub schema_version: u32,
    pub request_id: String,
    pub maintenance: BootstrapMaintenanceInput,
    pub candidate: BootstrapCandidateInput,
}

/// Exact current maintenance hold requested by the root Workspace coordinator.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapMaintenanceInput {
    pub transaction_id: String,
    pub maintenance_token: String,
    pub target_kind: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub component_artifact_digests: BTreeMap<String, String>,
    pub expected_gate_generation: u64,
    pub expected_catalog_generation: u64,
}

/// The package candidate whose official external proof was already checked by
/// Workspace and whose byte identity Platform independently verifies.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapCandidateInput {
    pub descriptor_path: PathBuf,
    pub archive_path: PathBuf,
    pub component_id: String,
    pub package_id: String,
    pub package_version: String,
    pub artifact_digest: String,
    pub archive_digest: String,
    pub descriptor_digest: String,
    pub manifest_digest: String,
    pub dependency_lock_digest: String,
}

/// Secret-free receipt of the Broker's exact current hold validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatedMaintenanceHold {
    pub valid: bool,
    pub request_id: String,
    pub target_kind: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub component_artifact_digests: BTreeMap<String, String>,
    pub component_id: String,
    pub artifact_digest: String,
    pub gate_generation: u64,
    pub catalog_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerResponse {
    request_id: String,
    #[serde(default)]
    result: Option<ValidatedMaintenanceHold>,
    #[serde(default)]
    error: Option<BrokerError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerError {
    code: String,
    #[serde(default)]
    message: Option<String>,
}

/// Secret-free result emitted by the one-shot parent.
#[derive(Debug, Serialize)]
pub struct BootstrapInstallReceipt {
    pub transaction_id: String,
    pub target_kind: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub component_artifact_digests: BTreeMap<String, String>,
    pub component_id: String,
    pub artifact_digest: String,
    pub expected_gate_generation: u64,
    pub expected_catalog_generation: u64,
    pub gate_generation: u64,
    pub catalog_generation: u64,
    pub installation: InstallationRecord,
}

/// Worker-only request. It deliberately contains no hold token or operator secret.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapWorkerInput {
    pub request_id: String,
    pub candidate: BootstrapCandidateInput,
}

impl BootstrapWorkerInput {
    /// Validates the secret-free install request before the unprivileged worker opens inputs.
    pub fn validate(&self) -> Result<OfflineInstallCandidateIdentity, PackageRuntimeError> {
        if !valid_broker_identifier(&self.request_id)
            || self.candidate.component_id != self.candidate.package_id
            || self.candidate.descriptor_path
                != Path::new(BOOTSTRAP_STAGE_ROOT)
                    .join(&self.request_id)
                    .join("descriptor.json")
            || self.candidate.archive_path
                != Path::new(BOOTSTRAP_STAGE_ROOT)
                    .join(&self.request_id)
                    .join("archive.zip")
        {
            return Err(invalid_bootstrap_input());
        }
        self.candidate.identity()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapWorkerOutput {
    pub request_id: String,
    pub installation: Option<InstallationRecord>,
    pub error: Option<BootstrapWorkerError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapWorkerError {
    pub code: String,
    pub message: String,
}

impl BootstrapInstallInput {
    /// Validates all fields whose authority is local to the signed package plan.
    pub fn validate(&self) -> Result<OfflineInstallCandidateIdentity, PackageRuntimeError> {
        if self.schema_version != 1 || !valid_broker_identifier(&self.request_id) {
            return Err(invalid_bootstrap_input());
        }
        let hold = &self.maintenance;
        if !valid_broker_identifier(&hold.transaction_id)
            || hold.maintenance_token.is_empty()
            || hold.maintenance_token.len() > MAX_MAINTENANCE_TOKEN_BYTES
            || hold.target_kind != "PACKAGE_ONLY"
            || !valid_broker_identifier(&hold.plan_id)
            || hold.plan_digest.is_empty()
            || hold.expected_gate_generation == 0
            || hold.expected_catalog_generation == 0
        {
            return Err(invalid_bootstrap_input());
        }
        ArtifactDigest::new(hold.plan_digest.clone()).map_err(|_| invalid_bootstrap_input())?;
        if hold.component_artifact_digests.is_empty()
            || hold.component_artifact_digests.iter().any(|(id, digest)| {
                !valid_broker_identifier(id) || ArtifactDigest::new(digest.clone()).is_err()
            })
        {
            return Err(invalid_bootstrap_input());
        }

        let candidate = &self.candidate;
        if candidate.component_id.is_empty()
            || candidate.component_id != candidate.package_id
            || hold.component_artifact_digests.get(&candidate.component_id)
                != Some(&candidate.artifact_digest)
            || candidate.descriptor_path
                != Path::new(PERSISTENT_BOOTSTRAP_ROOT)
                    .join(&self.request_id)
                    .join("descriptor.json")
            || candidate.archive_path
                != Path::new(PERSISTENT_BOOTSTRAP_ROOT)
                    .join(&self.request_id)
                    .join("archive.zip")
        {
            return Err(PackageRuntimeError::new(
                "OFFLINE_CANDIDATE_IDENTITY_MISMATCH",
                "candidate identity does not match its held package artifact",
            ));
        }

        candidate.identity()
    }
}

impl BootstrapCandidateInput {
    /// Converts untrusted JSON identity strings into validated Platform types.
    pub fn identity(&self) -> Result<OfflineInstallCandidateIdentity, PackageRuntimeError> {
        Ok(OfflineInstallCandidateIdentity {
            package_id: PackageId::new(self.package_id.clone())?,
            package_version: PackageVersion::new(self.package_version.clone())?,
            artifact_digest: ArtifactDigest::new(self.artifact_digest.clone())?,
            archive_digest: ArtifactDigest::new(self.archive_digest.clone())?,
            descriptor_digest: ArtifactDigest::new(self.descriptor_digest.clone())?,
            manifest_digest: ArtifactDigest::new(self.manifest_digest.clone())?,
            dependency_lock_digest: ArtifactDigest::new(self.dependency_lock_digest.clone())?,
        })
    }

    /// Returns the content source consumed by the Platform verifier.
    pub fn package_source(&self) -> PackageSource {
        PackageSource {
            descriptor_path: self.descriptor_path.clone(),
            archive_path: self.archive_path.clone(),
        }
    }
}

/// Reads a bounded one-shot JSON request from stdin.
pub fn read_bootstrap_stdin() -> Result<BootstrapInstallInput, PackageRuntimeError> {
    let bytes = read_bounded(io::stdin().lock(), MAX_BOOTSTRAP_INPUT_BYTES)?;
    parse_bootstrap_input(&bytes)
}

/// Reads a bounded root-owned, private bootstrap JSON file without following links.
pub fn read_bootstrap_file(path: &Path) -> Result<BootstrapInstallInput, PackageRuntimeError> {
    validate_bootstrap_input_file_location(path)?;
    let bytes = read_root_private_file(path, MAX_BOOTSTRAP_INPUT_BYTES)?;
    parse_bootstrap_input(&bytes)
}

/// Restricts private input files to the Workspace-owned persistent bootstrap request layout.
pub fn validate_bootstrap_input_file_location(path: &Path) -> Result<(), PackageRuntimeError> {
    let Some(parent) = path.parent() else {
        return Err(private_file_error());
    };
    let Some(request_id) = parent.file_name().and_then(|value| value.to_str()) else {
        return Err(private_file_error());
    };
    if path
        != Path::new(PERSISTENT_BOOTSTRAP_ROOT)
            .join(request_id)
            .join("request.json")
        || !valid_broker_identifier(request_id)
    {
        return Err(private_file_error());
    }
    for directory in [
        Path::new("/var/lib/cyrene-updates"),
        Path::new(PERSISTENT_BOOTSTRAP_ROOT),
        parent,
    ] {
        let metadata = fs::symlink_metadata(directory).map_err(|_| private_file_error())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(private_file_error());
        }
    }
    Ok(())
}

/// Parses and validates one strict bootstrap request.
pub fn parse_bootstrap_input(bytes: &[u8]) -> Result<BootstrapInstallInput, PackageRuntimeError> {
    if bytes.len() > MAX_BOOTSTRAP_INPUT_BYTES {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_INPUT_TOO_LARGE",
            "bootstrap input exceeds the 64 KiB limit",
        ));
    }
    let input: BootstrapInstallInput =
        serde_json::from_slice(bytes).map_err(|_| invalid_bootstrap_input())?;
    input.validate()?;
    Ok(input)
}

/// Reads the Broker's fixed root-only operator credential without exposing it.
pub fn read_operator_token() -> Result<String, PackageRuntimeError> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err(PackageRuntimeError::new(
            "ROOT_REQUIRED",
            "offline package bootstrap requires effective UID 0",
        ));
    }
    let bytes = read_root_private_file(Path::new(OPERATOR_TOKEN_FILE), MAX_OPERATOR_TOKEN_BYTES)?;
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| {
            PackageRuntimeError::new(
                "OPERATOR_AUTH_UNAVAILABLE",
                "operator credential is invalid",
            )
        })?
        .trim_end_matches(['\r', '\n'])
        .to_string();
    if token.is_empty()
        || token.len() > MAX_OPERATOR_TOKEN_BYTES
        || token.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(PackageRuntimeError::new(
            "OPERATOR_AUTH_UNAVAILABLE",
            "operator credential is invalid",
        ));
    }
    Ok(token)
}

/// Ensures source files are stable root-owned inputs and readable by the service UID.
pub fn validate_candidate_paths(
    candidate: &BootstrapCandidateInput,
    runtime_uid: u32,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    validate_candidate_file(&candidate.descriptor_path, runtime_uid, runtime_gid)?;
    validate_candidate_file(&candidate.archive_path, runtime_uid, runtime_gid)
}

/// Validates the root-only persistent source files before copying them to worker handoff.
pub fn validate_persistent_candidate_paths(
    request_id: &str,
    candidate: &BootstrapCandidateInput,
) -> Result<(), PackageRuntimeError> {
    let root = Path::new(PERSISTENT_BOOTSTRAP_ROOT).join(request_id);
    if candidate.descriptor_path != root.join("descriptor.json")
        || candidate.archive_path != root.join("archive.zip")
    {
        return Err(candidate_path_error());
    }
    for directory in [
        Path::new("/var/lib/cyrene-updates"),
        Path::new(PERSISTENT_BOOTSTRAP_ROOT),
        root.as_path(),
    ] {
        let metadata = fs::symlink_metadata(directory).map_err(|_| candidate_path_error())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(candidate_path_error());
        }
    }
    validate_root_private_candidate_file(&candidate.descriptor_path)?;
    validate_root_private_candidate_file(&candidate.archive_path)
}

/// Copies only the digest-pinned candidate bytes into the cyrene-readable transient handoff.
pub fn prepare_worker_candidate(
    request_id: &str,
    candidate: &BootstrapCandidateInput,
    runtime_gid: u32,
) -> Result<BootstrapCandidateInput, PackageRuntimeError> {
    if nix::unistd::geteuid().as_raw() != 0 || !valid_broker_identifier(request_id) {
        return Err(PackageRuntimeError::new(
            "ROOT_REQUIRED",
            "candidate handoff must be prepared by the root coordinator",
        ));
    }
    validate_persistent_candidate_paths(request_id, candidate)?;
    let stage_root = Path::new(BOOTSTRAP_STAGE_ROOT);
    validate_trusted_parent_chain(stage_root)?;
    ensure_handoff_directory(stage_root, 0, runtime_gid, 0o750)?;
    let request_directory = stage_root.join(request_id);
    ensure_handoff_directory(&request_directory, 0, runtime_gid, 0o750)?;

    let descriptor_path = request_directory.join("descriptor.json");
    let archive_path = request_directory.join("archive.zip");
    copy_or_validate_handoff_file(
        &candidate.descriptor_path,
        &descriptor_path,
        &candidate.descriptor_digest,
        MAX_CANDIDATE_DESCRIPTOR_BYTES,
        runtime_gid,
    )?;
    copy_or_validate_handoff_file(
        &candidate.archive_path,
        &archive_path,
        &candidate.archive_digest,
        MAX_CANDIDATE_ARCHIVE_BYTES,
        runtime_gid,
    )?;

    let mut staged = candidate.clone();
    staged.descriptor_path = descriptor_path;
    staged.archive_path = archive_path;
    Ok(staged)
}

/// Removes only the known root-owned handoff files after install and post-validation succeed.
pub fn cleanup_worker_candidate_handoff(
    request_id: &str,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    if !valid_broker_identifier(request_id) {
        return Err(candidate_path_error());
    }
    let stage_root = Path::new(BOOTSTRAP_STAGE_ROOT);
    validate_trusted_parent_chain(stage_root).map_err(|_| candidate_path_error())?;
    let directory = Path::new(BOOTSTRAP_STAGE_ROOT).join(request_id);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(candidate_path_error()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.gid() != runtime_gid
        || metadata.permissions().mode() & 0o777 != 0o750
    {
        return Err(candidate_path_error());
    }
    for name in ["descriptor.json", "archive.zip"] {
        let path = directory.join(name);
        let file = fs::symlink_metadata(&path).map_err(|_| candidate_path_error())?;
        if file.file_type().is_symlink()
            || !file.is_file()
            || file.uid() != 0
            || file.gid() != runtime_gid
            || file.nlink() != 1
            || file.permissions().mode() & 0o777 != 0o444
        {
            return Err(candidate_path_error());
        }
        fs::remove_file(path).map_err(|_| candidate_path_error())?;
    }
    fs::remove_dir(directory).map_err(|_| candidate_path_error())
}

/// Confirms the fixed runtime state directory is safe, creating it for cyrene only when absent.
pub fn ensure_runtime_state_root(
    runtime_uid: u32,
    runtime_gid: u32,
) -> Result<PathBuf, PackageRuntimeError> {
    let root = PathBuf::from(PACKAGE_RUNTIME_ROOT);
    validate_trusted_parent_chain(&root)?;
    match fs::symlink_metadata(&root) {
        Ok(_) => validate_runtime_state_root(&root, runtime_uid, runtime_gid)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&root).map_err(|_| runtime_root_error())?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&root)
                .map_err(|_| runtime_root_error())?;
            // SAFETY: this descriptor refers to the new directory beneath the
            // already validated root-owned /var/lib/cyrene parent.
            if unsafe { libc::fchown(directory.as_raw_fd(), runtime_uid, runtime_gid) } != 0
                || unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0
            {
                return Err(runtime_root_error());
            }
            validate_runtime_state_root(&root, runtime_uid, runtime_gid)?;
        }
        Err(_) => return Err(runtime_root_error()),
    }
    Ok(root)
}

/// Refuses bootstrap while a package-runtime listener is live.
pub fn ensure_runtime_daemon_stopped(runtime_uid: u32) -> Result<(), PackageRuntimeError> {
    let socket = Path::new(PACKAGE_RUNTIME_SOCKET);
    validate_runtime_socket_parent(runtime_uid)?;
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(runtime_daemon_state_error()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != runtime_uid
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(runtime_daemon_state_error());
    }
    match UnixStream::connect(socket) {
        Ok(_) => Err(PackageRuntimeError::new(
            "PACKAGE_RUNTIME_ALREADY_RUNNING",
            "package runtime control socket is still accepting connections",
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(runtime_daemon_state_error()),
    }
}

/// Validates the release-provisioned adapter and pinned tools before dropping to cyrene.
pub fn validate_bootstrap_toolchain(
    dependency_preparer: &Path,
    arguments: &[std::ffi::OsString],
) -> Result<(), PackageRuntimeError> {
    if dependency_preparer != Path::new(DEPENDENCY_PREPARER)
        || arguments
            != [
                std::ffi::OsString::from("--uv"),
                std::ffi::OsString::from(PINNED_UV),
                std::ffi::OsString::from("--python"),
                std::ffi::OsString::from(PINNED_PYTHON),
            ]
    {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_TOOLCHAIN_INVALID",
            "offline bootstrap requires the provisioned Plugin dependency adapter and pinned Python tools",
        ));
    }
    validate_root_executable(Path::new(DEPENDENCY_PREPARER))?;
    validate_root_executable(Path::new(PINNED_UV))?;
    validate_root_executable(Path::new(PINNED_PYTHON))
}

/// Authenticates the internal worker against the root parent that created its socketpair.
///
/// A same-UID caller can invoke the hidden worker mode, but cannot forge the root peer
/// credentials captured when this private channel was created.
pub fn verify_root_worker_peer(fd: RawFd) -> Result<(), PackageRuntimeError> {
    // SAFETY: this descriptor is passed only by the private one-shot parent channel.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let peer = getsockopt(&stream, PeerCredentials).map_err(|_| worker_authority_error())?;
    // SAFETY: getppid has no pointer arguments or side effects.
    let parent_pid = unsafe { libc::getppid() };
    if peer.uid() != 0 || peer.pid() != parent_pid {
        return Err(worker_authority_error());
    }
    Ok(())
}

/// Validates the Broker's current durable PACKAGE_ONLY hold using the real root peer.
pub fn validate_maintenance_hold(
    input: &BootstrapInstallInput,
    operator_token: &str,
) -> Result<ValidatedMaintenanceHold, PackageRuntimeError> {
    validate_maintenance_hold_at(input, operator_token, Path::new(BROKER_SOCKET), 0)
}

fn validate_maintenance_hold_at(
    input: &BootstrapInstallInput,
    operator_token: &str,
    broker_socket: &Path,
    expected_peer_uid: u32,
) -> Result<ValidatedMaintenanceHold, PackageRuntimeError> {
    input.validate()?;
    if operator_token.is_empty() || operator_token.len() > MAX_OPERATOR_TOKEN_BYTES {
        return Err(PackageRuntimeError::new(
            "OPERATOR_AUTH_UNAVAILABLE",
            "operator credential is invalid",
        ));
    }
    validate_broker_socket_path(broker_socket)?;
    let mut stream = UnixStream::connect(broker_socket).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_BROKER_UNAVAILABLE",
            "maintenance broker is unavailable",
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| broker_protocol_error())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| broker_protocol_error())?;
    let peer = getsockopt(&stream, PeerCredentials).map_err(|_| broker_protocol_error())?;
    if peer.uid() != expected_peer_uid {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_BROKER_PEER_INVALID",
            "maintenance broker peer is not the trusted root service",
        ));
    }

    let hold = &input.maintenance;
    let request = json!({
        "request_id": hold.transaction_id,
        "method": "ValidateMaintenanceHold",
        "protocol_version": BROKER_PROTOCOL_VERSION,
        "auth": {"operator_token": operator_token},
        "params": {
            "request_id": hold.transaction_id,
            "maintenance_token": hold.maintenance_token,
            "target_kind": hold.target_kind,
            "plan_id": hold.plan_id,
            "plan_digest": hold.plan_digest,
            "component_artifact_digests": hold.component_artifact_digests,
            "component_id": input.candidate.component_id,
            "artifact_digest": input.candidate.artifact_digest,
            "expected_gate_generation": hold.expected_gate_generation,
            "expected_catalog_generation": hold.expected_catalog_generation
        }
    });
    let mut request_bytes = serde_json::to_vec(&request).map_err(|_| broker_protocol_error())?;
    request_bytes.push(b'\n');
    stream
        .write_all(&request_bytes)
        .map_err(|_| broker_protocol_error())?;

    let response_bytes = read_bounded(&mut stream, MAX_BROKER_FRAME_BYTES)?;
    let response = parse_broker_response(&response_bytes, &hold.transaction_id, input)?;
    Ok(response)
}

fn parse_broker_response(
    bytes: &[u8],
    broker_request_id: &str,
    input: &BootstrapInstallInput,
) -> Result<ValidatedMaintenanceHold, PackageRuntimeError> {
    let response: BrokerResponse =
        serde_json::from_slice(bytes).map_err(|_| broker_protocol_error())?;
    if response.request_id != broker_request_id {
        return Err(broker_protocol_error());
    }
    let validated = match (response.result, response.error) {
        (Some(result), None) => result,
        (None, Some(error)) => {
            let _ignored_broker_message = error.message;
            let code = if valid_error_code(&error.code) {
                error.code
            } else {
                "MAINTENANCE_ADMISSION_DENIED".to_string()
            };
            return Err(PackageRuntimeError::new(
                code,
                "maintenance broker rejected the current hold",
            ));
        }
        _ => return Err(broker_protocol_error()),
    };
    let hold = &input.maintenance;
    if !validated.valid
        || validated.request_id != hold.transaction_id
        || validated.target_kind != "PACKAGE_ONLY"
        || validated.plan_id != hold.plan_id
        || validated.plan_digest != hold.plan_digest
        || validated.component_artifact_digests != hold.component_artifact_digests
        || validated.component_id != input.candidate.component_id
        || validated.artifact_digest != input.candidate.artifact_digest
        || validated.gate_generation != hold.expected_gate_generation
        || validated.catalog_generation != hold.expected_catalog_generation
    {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_ADMISSION_DENIED",
            "maintenance broker did not confirm the exact current package hold",
        ));
    }
    Ok(validated)
}

fn validate_broker_socket_path(path: &Path) -> Result<(), PackageRuntimeError> {
    validate_trusted_parent_chain(path).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_BROKER_PEER_INVALID",
            "maintenance broker socket parent is not trusted",
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        PackageRuntimeError::new(
            "MAINTENANCE_BROKER_UNAVAILABLE",
            "maintenance broker socket is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o002 != 0
    {
        return Err(PackageRuntimeError::new(
            "MAINTENANCE_BROKER_PEER_INVALID",
            "maintenance broker socket path is not trusted",
        ));
    }
    Ok(())
}

fn validate_candidate_file(
    path: &Path,
    runtime_uid: u32,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(candidate_path_error());
    }
    let mut current = PathBuf::from("/");
    let components = path.components().collect::<Vec<_>>();
    for component in &components[..components.len().saturating_sub(1)] {
        if let Component::Normal(part) = component {
            current.push(part);
            let metadata = fs::symlink_metadata(&current).map_err(|_| candidate_path_error())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(candidate_path_error());
            }
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| candidate_path_error())?;
    let mode = metadata.permissions().mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != 0
        || mode & 0o022 != 0
        || !mode_readable_by(
            mode,
            metadata.uid(),
            metadata.gid(),
            runtime_uid,
            runtime_gid,
        )
    {
        return Err(candidate_path_error());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| candidate_path_error())?;
    let opened = file.metadata().map_err(|_| candidate_path_error())?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(candidate_path_error());
    }
    Ok(())
}

fn validate_root_private_candidate_file(path: &Path) -> Result<(), PackageRuntimeError> {
    validate_trusted_parent_chain(path).map_err(|_| candidate_path_error())?;
    let metadata = fs::symlink_metadata(path).map_err(|_| candidate_path_error())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(candidate_path_error());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| candidate_path_error())?;
    let opened = file.metadata().map_err(|_| candidate_path_error())?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(candidate_path_error());
    }
    Ok(())
}

fn ensure_handoff_directory(
    path: &Path,
    owner_uid: u32,
    owner_gid: u32,
    mode: u32,
) -> Result<(), PackageRuntimeError> {
    validate_trusted_parent_chain(path).map_err(|_| candidate_path_error())?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != owner_uid
                || metadata.gid() != owner_gid
                || metadata.permissions().mode() & 0o777 != mode
            {
                return Err(candidate_path_error());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| candidate_path_error())?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(path)
                .map_err(|_| candidate_path_error())?;
            // SAFETY: this descriptor refers to the new directory beneath a validated root path.
            if unsafe { libc::fchown(directory.as_raw_fd(), owner_uid, owner_gid) } != 0
                || unsafe { libc::fchmod(directory.as_raw_fd(), mode) } != 0
            {
                return Err(candidate_path_error());
            }
            let metadata = directory.metadata().map_err(|_| candidate_path_error())?;
            if metadata.uid() != owner_uid
                || metadata.gid() != owner_gid
                || metadata.permissions().mode() & 0o777 != mode
            {
                return Err(candidate_path_error());
            }
        }
        Err(_) => return Err(candidate_path_error()),
    }
    Ok(())
}

fn copy_or_validate_handoff_file(
    source_path: &Path,
    destination_path: &Path,
    expected_digest: &str,
    maximum_bytes: u64,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    let source_metadata = fs::symlink_metadata(source_path).map_err(|_| candidate_path_error())?;
    if source_metadata.file_type().is_symlink()
        || !source_metadata.is_file()
        || source_metadata.uid() != 0
        || source_metadata.nlink() != 1
        || source_metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(candidate_path_error());
    }
    if source_metadata.len() > maximum_bytes {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_CANDIDATE_TOO_LARGE",
            "package candidate exceeds the configured bootstrap limit",
        ));
    }
    match fs::symlink_metadata(destination_path) {
        Ok(metadata) => {
            validate_handoff_file_metadata(&metadata, runtime_gid)?;
            if metadata.len() > maximum_bytes
                || digest_path(destination_path)? != expected_digest
                || digest_path(source_path)? != expected_digest
            {
                return Err(candidate_path_error());
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(candidate_path_error()),
    }

    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source_path)
        .map_err(|_| candidate_path_error())?;
    let opened_source = source.metadata().map_err(|_| candidate_path_error())?;
    if opened_source.dev() != source_metadata.dev() || opened_source.ino() != source_metadata.ino()
    {
        return Err(candidate_path_error());
    }
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(destination_path)
        .map_err(|_| candidate_path_error())?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|_| candidate_path_error())?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > maximum_bytes {
                return Err(PackageRuntimeError::new(
                    "BOOTSTRAP_CANDIDATE_TOO_LARGE",
                    "package candidate exceeds the configured bootstrap limit",
                ));
            }
            hasher.update(&buffer[..read]);
            destination
                .write_all(&buffer[..read])
                .map_err(|_| candidate_path_error())?;
        }
        destination.sync_all().map_err(|_| candidate_path_error())?;
        // SAFETY: the descriptor is a newly created root-owned handoff file.
        if unsafe { libc::fchown(destination.as_raw_fd(), 0, runtime_gid) } != 0
            || unsafe { libc::fchmod(destination.as_raw_fd(), 0o444) } != 0
        {
            return Err(candidate_path_error());
        }
        let actual_digest = format!("sha256:{:x}", hasher.finalize());
        if actual_digest != expected_digest {
            return Err(PackageRuntimeError::new(
                "OFFLINE_CANDIDATE_IDENTITY_MISMATCH",
                "candidate bytes do not match the held artifact digest",
            ));
        }
        let metadata = destination.metadata().map_err(|_| candidate_path_error())?;
        validate_handoff_file_metadata(&metadata, runtime_gid)
    })();
    drop(destination);
    if result.is_err() {
        let _ = fs::remove_file(destination_path);
    }
    result
}

fn validate_handoff_file_metadata(
    metadata: &fs::Metadata,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != runtime_gid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o444
    {
        return Err(candidate_path_error());
    }
    Ok(())
}

fn digest_path(path: &Path) -> Result<String, PackageRuntimeError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| candidate_path_error())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| candidate_path_error())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn mode_readable_by(
    mode: u32,
    file_uid: u32,
    file_gid: u32,
    runtime_uid: u32,
    runtime_gid: u32,
) -> bool {
    if runtime_uid == 0 {
        true
    } else if runtime_uid == file_uid {
        mode & 0o400 != 0
    } else if runtime_gid == file_gid {
        mode & 0o040 != 0
    } else {
        mode & 0o004 != 0
    }
}

fn read_root_private_file(path: &Path, limit: usize) -> Result<Vec<u8>, PackageRuntimeError> {
    if !path.is_absolute() {
        return Err(private_file_error());
    }
    let mut current = PathBuf::from("/");
    let components = path.components().collect::<Vec<_>>();
    for component in &components[..components.len().saturating_sub(1)] {
        if let Component::Normal(part) = component {
            current.push(part);
            let metadata = fs::symlink_metadata(&current).map_err(|_| private_file_error())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(private_file_error());
            }
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| private_file_error())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(private_file_error());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| private_file_error())?;
    let opened = file.metadata().map_err(|_| private_file_error())?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(private_file_error());
    }
    read_bounded(&mut file, limit)
}

fn validate_trusted_parent_chain(path: &Path) -> Result<(), PackageRuntimeError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(runtime_root_error());
    }
    let components = path.components().collect::<Vec<_>>();
    let mut current = PathBuf::from("/");
    for component in &components[..components.len().saturating_sub(1)] {
        if let Component::Normal(part) = component {
            current.push(part);
            let metadata = fs::symlink_metadata(&current).map_err(|_| runtime_root_error())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(runtime_root_error());
            }
        }
    }
    Ok(())
}

fn validate_runtime_socket_parent(runtime_uid: u32) -> Result<(), PackageRuntimeError> {
    let socket = Path::new(PACKAGE_RUNTIME_SOCKET);
    let parent = socket.parent().ok_or_else(runtime_daemon_state_error)?;
    validate_trusted_parent_chain(parent)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| runtime_daemon_state_error())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || (metadata.uid() != 0 && metadata.uid() != runtime_uid)
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(runtime_daemon_state_error());
    }
    Ok(())
}

fn validate_runtime_state_root(
    root: &Path,
    runtime_uid: u32,
    runtime_gid: u32,
) -> Result<(), PackageRuntimeError> {
    let metadata = fs::symlink_metadata(root).map_err(|_| runtime_root_error())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != runtime_uid
        || metadata.gid() != runtime_gid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(runtime_root_error());
    }
    Ok(())
}

fn validate_root_executable(path: &Path) -> Result<(), PackageRuntimeError> {
    validate_trusted_parent_chain(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| toolchain_error())?;
    let mode = metadata.permissions().mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || mode & 0o022 != 0
        || mode & 0o111 == 0
    {
        return Err(toolchain_error());
    }
    Ok(())
}

fn valid_broker_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn runtime_root_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "RUNTIME_ROOT_UNAVAILABLE",
        "package runtime root is missing or has unsafe ownership or permissions",
    )
}

fn runtime_daemon_state_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "PACKAGE_RUNTIME_STATE_UNSAFE",
        "package runtime socket state is unsafe or could not be cleaned safely",
    )
}

fn toolchain_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_TOOLCHAIN_INVALID",
        "a pinned package dependency tool is unavailable or has unsafe ownership",
    )
}

fn worker_authority_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_WORKER_UNAUTHORIZED",
        "offline installer worker was not started by its root coordinator",
    )
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, PackageRuntimeError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            PackageRuntimeError::new(
                "BOOTSTRAP_INPUT_UNAVAILABLE",
                "bounded input could not be read",
            )
        })?;
    if bytes.len() > limit {
        return Err(PackageRuntimeError::new(
            "BOOTSTRAP_INPUT_TOO_LARGE",
            "input exceeds its protocol size limit",
        ));
    }
    Ok(bytes)
}

fn invalid_bootstrap_input() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_INPUT_INVALID",
        "bootstrap input is malformed or outside protocol limits",
    )
}

fn private_file_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_PRIVATE_FILE_INVALID",
        "private bootstrap or operator file has unsafe ownership or permissions",
    )
}

fn candidate_path_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "BOOTSTRAP_CANDIDATE_PATH_INVALID",
        "candidate files must be stable root-owned regular files readable by the runtime account",
    )
}

fn broker_protocol_error() -> PackageRuntimeError {
    PackageRuntimeError::new(
        "MAINTENANCE_BROKER_PROTOCOL_INVALID",
        "maintenance broker response did not match the hold validation protocol",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn valid_input() -> Value {
        json!({
            "schema_version": 1,
            "request_id": "install-request-1",
            "maintenance": {
                "transaction_id": "package-hold-1",
                "maintenance_token": "private-maintenance-token",
                "target_kind": "PACKAGE_ONLY",
                "plan_id": "package-plan-1",
                "plan_digest": DIGEST_A,
                "component_artifact_digests": {"org.cyrene.example-plugin": DIGEST_B},
                "expected_gate_generation": 5,
                "expected_catalog_generation": 9
            },
            "candidate": {
                "descriptor_path": "/var/lib/cyrene-updates/plugin-package-bootstrap/install-request-1/descriptor.json",
                "archive_path": "/var/lib/cyrene-updates/plugin-package-bootstrap/install-request-1/archive.zip",
                "component_id": "org.cyrene.example-plugin",
                "package_id": "org.cyrene.example-plugin",
                "package_version": "1.2.3",
                "artifact_digest": DIGEST_B,
                "archive_digest": DIGEST_A,
                "descriptor_digest": DIGEST_A,
                "manifest_digest": DIGEST_B,
                "dependency_lock_digest": DIGEST_A
            }
        })
    }

    #[test]
    fn bootstrap_input_binds_package_identity_and_exact_plan_digest() {
        let input: BootstrapInstallInput =
            serde_json::from_value(valid_input()).expect("valid bootstrap request");
        assert!(input.validate().is_ok());

        let mut mismatch = valid_input();
        mismatch["candidate"]["component_id"] = json!("cy-package-runtime");
        assert!(
            BootstrapInstallInput::deserialize(mismatch)
                .unwrap()
                .validate()
                .is_err()
        );

        let mut digest_mismatch = valid_input();
        digest_mismatch["candidate"]["artifact_digest"] = json!(DIGEST_A);
        assert!(
            BootstrapInstallInput::deserialize(digest_mismatch)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn bootstrap_input_rejects_unknown_fields_and_oversized_frames() {
        let mut unknown = valid_input();
        unknown["trust_passed"] = json!(true);
        assert!(parse_bootstrap_input(&serde_json::to_vec(&unknown).unwrap()).is_err());
        assert!(parse_bootstrap_input(&vec![b' '; MAX_BOOTSTRAP_INPUT_BYTES + 1]).is_err());
    }

    #[test]
    fn broker_response_must_confirm_every_current_hold_field() {
        let input: BootstrapInstallInput =
            serde_json::from_value(valid_input()).expect("valid bootstrap request");
        let response = json!({
            "request_id": "package-hold-1",
            "result": {
                "valid": true,
                "request_id": "package-hold-1",
                "target_kind": "PACKAGE_ONLY",
                "plan_id": "package-plan-1",
                "plan_digest": DIGEST_A,
                "component_artifact_digests": {"org.cyrene.example-plugin": DIGEST_B},
                "component_id": "org.cyrene.example-plugin",
                "artifact_digest": DIGEST_B,
                "gate_generation": 5,
                "catalog_generation": 9
            }
        });
        let validated = parse_broker_response(
            &serde_json::to_vec(&response).unwrap(),
            "package-hold-1",
            &input,
        )
        .unwrap();
        assert_eq!(validated.catalog_generation, 9);

        let mut mismatch = response;
        mismatch["result"]["catalog_generation"] = json!(10);
        assert!(
            parse_broker_response(
                &serde_json::to_vec(&mismatch).unwrap(),
                "package-hold-1",
                &input,
            )
            .is_err()
        );
    }
}
