//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 config.rs                                                       │
//! │  Module: cy_runtime_agent::config                                   │
//! │  Role: Runtime Agent immutable launch configuration.                │
//! │                                                                     │
//! │  模块职责：校验控制面、身份、证书、路径与固定 workload 命令。             │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cy_kernel_contract::Identity;
use cy_proto::core_v1::NodeRef;
use rustix::fs::{
    fchmod, flock, fsync, open, openat, renameat, unlinkat, AtFlags, FlockOperation, Mode, OFlags,
};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::RuntimeAgentError;

const RESUME_TOKEN_STATE_FILE_PREFIX: &str = "runtime-resume-token-";
const RESUME_TOKEN_STATE_VERSION: u32 = 1;
const RESUME_TOKEN_NAMESPACE_VERSION: &[u8] = b"cyrene.runtime-agent.resume-token.v1";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedResumeToken {
    schema_version: u32,
    runtime_id: String,
    runtime_generation: u64,
    node_id: String,
    node_epoch: u64,
    resume_token: String,
}

/// Open handle for one private Agent state directory.
/// 单个私有 Agent 状态目录的已打开句柄。
///
/// All credential reads and replacements are relative to this descriptor, so
/// swapping the configured path after startup cannot redirect state I/O.
pub(crate) struct StateDirectory {
    directory: File,
    _runtime_lock: File,
}

/// Immutable launch-time configuration. Control messages cannot replace the command.
#[derive(Debug, Clone)]
pub struct RuntimeAgentConfig {
    pub control_plane_endpoint: String,
    pub control_plane_server_name: String,
    pub control_plane_ca: PathBuf,
    pub client_certificate: PathBuf,
    pub client_key: PathBuf,
    pub artifact_ca: PathBuf,
    pub artifact_ticket_key: Option<PathBuf>,
    pub organization_id: String,
    pub workspace_id: String,
    pub node: NodeRef,
    pub node_type: String,
    pub persistent: bool,
    pub runtime: Identity,
    pub agent_version: String,
    pub enrollment_proof: String,
    pub resume_token: String,
    pub state_dir: PathBuf,
    pub artifact_destination_root: PathBuf,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    pub workload: Vec<String>,
}

impl RuntimeAgentConfig {
    pub fn validate(&self) -> Result<(), RuntimeAgentError> {
        self.runtime.validate().map_err(|error| {
            RuntimeAgentError::Configuration(format!("{}: {}", error.reason_code, error.message))
        })?;
        if self.node.node_id.is_empty() || self.node.node_epoch == 0 || self.node_type.is_empty() {
            return Err(RuntimeAgentError::Configuration(
                "NodeId, positive Node epoch, and node type are required".to_string(),
            ));
        }
        if !self.control_plane_endpoint.starts_with("https://")
            || self.control_plane_server_name.is_empty()
        {
            return Err(RuntimeAgentError::Configuration(
                "control-plane endpoint must use HTTPS with an explicit server name".to_string(),
            ));
        }
        if self.organization_id.is_empty()
            || self.workspace_id.is_empty()
            || self.agent_version.is_empty()
        {
            return Err(RuntimeAgentError::Configuration(
                "organization, workspace, and agent version are required".to_string(),
            ));
        }
        if self.workload.is_empty() || self.workload[0].is_empty() {
            return Err(RuntimeAgentError::Configuration(
                "a preconfigured workload command is required after --".to_string(),
            ));
        }
        if !self.state_dir.is_absolute() || !self.artifact_destination_root.is_absolute() {
            return Err(RuntimeAgentError::Configuration(
                "state and Artifact destination roots must be absolute".to_string(),
            ));
        }
        if self.reconnect_min.is_zero()
            || self.reconnect_max.is_zero()
            || self.reconnect_min > self.reconnect_max
        {
            return Err(RuntimeAgentError::Configuration(
                "reconnect bounds must be non-zero and ordered".to_string(),
            ));
        }
        Ok(())
    }

    /// Create and open the credential state directory with owner-only access.
    /// 创建凭据状态目录并以仅属主权限打开。
    pub(crate) fn prepare_state_directory(&self) -> Result<StateDirectory, RuntimeAgentError> {
        fs::create_dir_all(&self.state_dir)
            .map_err(|error| state_io_error(&self.state_dir, error))?;
        let raw_directory = open(
            &self.state_dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| rustix_state_error(&self.state_dir, error))?;
        let directory = File::from(raw_directory);
        let metadata = directory
            .metadata()
            .map_err(|error| state_io_error(&self.state_dir, error))?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
        {
            return Err(RuntimeAgentError::State(format!(
                "{}: state directory must be a non-symlink directory owned by the effective user",
                self.state_dir.display()
            )));
        }
        fchmod(&directory, Mode::from_bits_truncate(0o700))
            .map_err(|error| rustix_state_error(&self.state_dir, error))?;
        let mode = directory
            .metadata()
            .map_err(|error| state_io_error(&self.state_dir, error))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o700 {
            return Err(RuntimeAgentError::State(format!(
                "{}: state directory permissions must be 0700, found {mode:04o}",
                self.state_dir.display()
            )));
        }
        let lock_name = self.resume_token_lock_name();
        let lock_path = self.state_dir.join(&lock_name);
        let raw_lock = openat(
            &directory,
            lock_name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|error| rustix_state_error(&lock_path, error))?;
        let lock_file = File::from(raw_lock);
        fchmod(&lock_file, Mode::from_bits_truncate(0o600))
            .map_err(|error| rustix_state_error(&lock_path, error))?;
        validate_private_regular_file(&lock_path, &lock_file, "Runtime state lock")?;
        flock(&lock_file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            RuntimeAgentError::State(format!(
                "{}: another Runtime Agent already owns this Runtime/Node state lock: {}",
                lock_path.display(),
                std::io::Error::from(error)
            ))
        })?;
        Ok(StateDirectory {
            directory,
            _runtime_lock: lock_file,
        })
    }

    /// Resolve the reconnect credential from durable state before the first Hello.
    /// 从 durable state 解析首次 Hello 使用的重连凭证。
    ///
    /// A configured token is treated as an explicit credential. If durable state exists,
    /// it must match exactly; otherwise a stale or cross-generation token could be sent.
    pub(crate) fn resolve_resume_token(
        &self,
        state_directory: &StateDirectory,
    ) -> Result<String, RuntimeAgentError> {
        let persisted = self.load_persisted_resume_token(state_directory)?;
        if let Some(persisted) = persisted {
            if !self.resume_token.is_empty() && self.resume_token != persisted.resume_token {
                return Err(RuntimeAgentError::Configuration(
                    "configured resume token conflicts with persisted runtime state".to_string(),
                ));
            }
            return Ok(persisted.resume_token);
        }
        if !self.resume_token.is_empty() {
            return Ok(self.resume_token.clone());
        }
        if self.enrollment_proof.is_empty() {
            return Err(RuntimeAgentError::Configuration(
                "enrollment proof or persisted resume token is required".to_string(),
            ));
        }
        Ok(String::new())
    }

    /// Persist a Welcome token without ever writing the one-shot enrollment proof.
    /// 持久化 Welcome token，但永不写入一次性 enrollment proof。
    pub(crate) fn persist_resume_token(
        &self,
        state_directory: &StateDirectory,
        resume_token: &str,
    ) -> Result<(), RuntimeAgentError> {
        if resume_token.is_empty() {
            return Err(RuntimeAgentError::State(
                "cannot persist an empty resume token".to_string(),
            ));
        }
        let state = PersistedResumeToken {
            schema_version: RESUME_TOKEN_STATE_VERSION,
            runtime_id: self.runtime.id.clone(),
            runtime_generation: self.runtime.generation,
            node_id: self.node.node_id.clone(),
            node_epoch: self.node.node_epoch,
            resume_token: resume_token.to_string(),
        };
        let encoded = serde_json::to_vec(&state)
            .map_err(|error| RuntimeAgentError::State(error.to_string()))?;
        let destination_name = self.resume_token_state_name();
        let temporary_name = self.resume_token_temp_name();
        let destination = self.state_dir.join(&destination_name);
        let temporary = self.state_dir.join(&temporary_name);
        let result = (|| {
            let raw_file = openat(
                &state_directory.directory,
                temporary_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|error| rustix_state_error(&temporary, error))?;
            let mut file = File::from(raw_file);
            validate_state_file(&temporary, &file)?;
            file.write_all(&encoded)
                .map_err(|error| state_io_error(&temporary, error))?;
            file.sync_all()
                .map_err(|error| state_io_error(&temporary, error))?;
            fchmod(&file, Mode::from_bits_truncate(0o600))
                .map_err(|error| rustix_state_error(&temporary, error))?;
            validate_state_file(&temporary, &file)?;
            drop(file);
            renameat(
                &state_directory.directory,
                temporary_name.as_str(),
                &state_directory.directory,
                destination_name.as_str(),
            )
            .map_err(|error| rustix_state_error(&destination, error))?;
            fsync(&state_directory.directory)
                .map_err(|error| rustix_state_error(&self.state_dir, error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(
                &state_directory.directory,
                temporary_name.as_str(),
                AtFlags::empty(),
            );
        }
        result
    }

    fn load_persisted_resume_token(
        &self,
        state_directory: &StateDirectory,
    ) -> Result<Option<PersistedResumeToken>, RuntimeAgentError> {
        let name = self.resume_token_state_name();
        let path = self.state_dir.join(&name);
        let raw_file = match openat(
            &state_directory.directory,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(raw_file) => raw_file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(rustix_state_error(&path, error)),
        };
        let mut file = File::from(raw_file);
        validate_state_file(&path, &file)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .map_err(|error| state_io_error(&path, error))?;
        let state: PersistedResumeToken = serde_json::from_slice(&bytes).map_err(|error| {
            RuntimeAgentError::State(format!(
                "{}: invalid persisted resume state: {error}",
                path.display()
            ))
        })?;
        if state.schema_version != RESUME_TOKEN_STATE_VERSION || state.resume_token.is_empty() {
            return Err(RuntimeAgentError::State(
                "persisted resume state has an unsupported schema or empty token".to_string(),
            ));
        }
        if state.runtime_id != self.runtime.id
            || state.runtime_generation != self.runtime.generation
        {
            return Err(RuntimeAgentError::Configuration(
                "persisted resume state does not match Runtime identity/generation".to_string(),
            ));
        }
        if state.node_id != self.node.node_id || state.node_epoch != self.node.node_epoch {
            return Err(RuntimeAgentError::Configuration(
                "persisted resume state does not match NodeRef".to_string(),
            ));
        }
        Ok(Some(state))
    }

    #[cfg(test)]
    fn resume_token_state_path(&self) -> PathBuf {
        self.state_dir.join(self.resume_token_state_name())
    }

    fn resume_token_state_name(&self) -> String {
        format!(
            "{RESUME_TOKEN_STATE_FILE_PREFIX}{}.json",
            resume_token_namespace(&self.runtime, &self.node)
        )
    }

    fn resume_token_temp_name(&self) -> String {
        format!(
            ".{RESUME_TOKEN_STATE_FILE_PREFIX}{}.{}.tmp",
            resume_token_namespace(&self.runtime, &self.node),
            Uuid::new_v4()
        )
    }

    fn resume_token_lock_name(&self) -> String {
        format!(
            ".{RESUME_TOKEN_STATE_FILE_PREFIX}{}.lock",
            resume_token_namespace(&self.runtime, &self.node)
        )
    }
}

fn resume_token_namespace(runtime: &Identity, node: &NodeRef) -> String {
    let mut hasher = Sha256::new();
    hasher.update(RESUME_TOKEN_NAMESPACE_VERSION);
    hash_string(&mut hasher, &runtime.id);
    hasher.update(runtime.generation.to_be_bytes());
    hash_string(&mut hasher, &node.node_id);
    hasher.update(node.node_epoch.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn state_io_error(path: &Path, error: std::io::Error) -> RuntimeAgentError {
    RuntimeAgentError::State(format!("{}: {error}", path.display()))
}

fn rustix_state_error(path: &Path, error: Errno) -> RuntimeAgentError {
    state_io_error(path, std::io::Error::from(error))
}

fn validate_state_file(path: &Path, file: &File) -> Result<(), RuntimeAgentError> {
    validate_private_regular_file(path, file, "resume state")
}

fn validate_private_regular_file(
    path: &Path,
    file: &File,
    description: &str,
) -> Result<(), RuntimeAgentError> {
    let metadata = file
        .metadata()
        .map_err(|error| state_io_error(path, error))?;
    let mode = metadata.permissions().mode() & 0o777;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.file_type().is_fifo()
        || metadata.uid() != geteuid().as_raw()
        || mode != 0o600
    {
        return Err(RuntimeAgentError::State(format!(
            "{}: {description} must be a regular owner-only 0600 file",
            path.display(),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    fn test_config(state_dir: PathBuf) -> RuntimeAgentConfig {
        RuntimeAgentConfig {
            control_plane_endpoint: "https://control.example".to_string(),
            control_plane_server_name: "control.example".to_string(),
            control_plane_ca: PathBuf::from("/tmp/control-ca.pem"),
            client_certificate: PathBuf::from("/tmp/client.pem"),
            client_key: PathBuf::from("/tmp/client.key"),
            artifact_ca: PathBuf::from("/tmp/artifact-ca.pem"),
            artifact_ticket_key: None,
            organization_id: "org-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            node: NodeRef {
                node_id: "node-1".to_string(),
                node_epoch: 7,
            },
            node_type: "container-host".to_string(),
            persistent: true,
            runtime: Identity {
                id: "runtime-1".to_string(),
                generation: 3,
            },
            agent_version: "test".to_string(),
            enrollment_proof: "one-shot-proof".to_string(),
            resume_token: String::new(),
            state_dir,
            artifact_destination_root: PathBuf::from("/tmp/artifacts"),
            reconnect_min: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(5),
            workload: vec!["/bin/true".to_string()],
        }
    }

    #[test]
    fn welcome_token_round_trips_without_persisting_enrollment_proof() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        config
            .persist_resume_token(&state_directory, "resume-1")
            .unwrap();

        assert_eq!(
            config.resolve_resume_token(&state_directory).unwrap(),
            "resume-1"
        );
        let state = fs::read_to_string(config.resume_token_state_path()).unwrap();
        assert!(state.contains("resume-1"));
        assert!(!state.contains("one-shot-proof"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(config.resume_token_state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn persisted_state_content_must_match_runtime_and_node_identity() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        config
            .persist_resume_token(&state_directory, "resume-1")
            .unwrap();

        let mut other_runtime = config.clone();
        other_runtime.runtime.generation += 1;
        fs::copy(
            config.resume_token_state_path(),
            other_runtime.resume_token_state_path(),
        )
        .unwrap();
        assert!(matches!(
            other_runtime.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("Runtime identity/generation")
        ));

        let mut other_node = config.clone();
        other_node.node.node_epoch += 1;
        fs::copy(
            config.resume_token_state_path(),
            other_node.resume_token_state_path(),
        )
        .unwrap();
        assert!(matches!(
            other_node.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("NodeRef")
        ));
    }

    #[test]
    fn state_namespace_isolates_generation_and_node_but_allows_original_recovery() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        config
            .persist_resume_token(&state_directory, "resume-1")
            .unwrap();

        let mut other_runtime = config.clone();
        other_runtime.runtime.generation += 1;
        other_runtime.enrollment_proof.clear();
        assert!(matches!(
            other_runtime.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("enrollment proof or persisted resume token")
        ));
        assert_ne!(
            config.resume_token_state_path(),
            other_runtime.resume_token_state_path()
        );

        let mut other_node = config.clone();
        other_node.node.node_epoch += 1;
        other_node.enrollment_proof.clear();
        assert!(matches!(
            other_node.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("enrollment proof or persisted resume token")
        ));
        assert_ne!(
            config.resume_token_state_path(),
            other_node.resume_token_state_path()
        );

        assert_eq!(
            config.resolve_resume_token(&state_directory).unwrap(),
            "resume-1"
        );
    }

    #[test]
    fn configured_token_must_match_persisted_token() {
        let directory = tempdir().unwrap();
        let mut config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        config
            .persist_resume_token(&state_directory, "resume-1")
            .unwrap();
        config.resume_token = "resume-stale".to_string();

        assert!(matches!(
            config.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("conflicts with persisted")
        ));
    }

    #[test]
    fn initial_enrollment_is_allowed_only_without_persisted_state() {
        let directory = tempdir().unwrap();
        let mut config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        assert_eq!(config.resolve_resume_token(&state_directory).unwrap(), "");
        config.enrollment_proof.clear();
        assert!(matches!(
            config.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("enrollment proof or persisted resume token")
        ));
    }

    #[test]
    fn credential_resolution_accepts_persisted_token_but_rejects_missing_credentials() {
        let directory = tempdir().unwrap();
        let mut config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        config.enrollment_proof.clear();
        assert!(matches!(
            config.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::Configuration(message))
                if message.contains("enrollment proof or persisted resume token")
        ));

        config
            .persist_resume_token(&state_directory, "resume-1")
            .unwrap();
        assert_eq!(
            config.resolve_resume_token(&state_directory).unwrap(),
            "resume-1"
        );
    }

    #[test]
    fn malformed_persisted_state_fails_closed() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        fs::write(config.resume_token_state_path(), b"not-json").unwrap();
        fs::set_permissions(
            config.resume_token_state_path(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        assert!(matches!(
            config.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::State(message))
                if message.contains("invalid persisted resume state")
        ));
    }

    #[test]
    fn state_directory_is_private_and_rejects_a_symlink() {
        let directory = tempdir().unwrap();
        let state_path = directory.path().join("state");
        fs::create_dir(&state_path).unwrap();
        fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o777)).unwrap();
        let config = test_config(state_path.clone());
        let state_directory = config.prepare_state_directory().unwrap();
        assert_eq!(
            state_directory
                .directory
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(state_directory);

        let link = directory.path().join("state-link");
        std::os::unix::fs::symlink(&state_path, &link).unwrap();
        let linked_config = test_config(link);
        assert!(matches!(
            linked_config.prepare_state_directory(),
            Err(RuntimeAgentError::State(_))
        ));
    }

    #[test]
    fn resume_state_symlink_is_rejected_without_following_it() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let state_directory = config.prepare_state_directory().unwrap();
        let outside = directory.path().join("outside-token");
        fs::write(&outside, b"not-a-token").unwrap();
        std::os::unix::fs::symlink(&outside, config.resume_token_state_path()).unwrap();

        assert!(matches!(
            config.resolve_resume_token(&state_directory),
            Err(RuntimeAgentError::State(_))
        ));
    }

    #[test]
    fn a_second_agent_cannot_own_the_same_runtime_state() {
        let directory = tempdir().unwrap();
        let config = test_config(directory.path().to_path_buf());
        let _first = config.prepare_state_directory().unwrap();

        assert!(matches!(
            config.prepare_state_directory(),
            Err(RuntimeAgentError::State(message)) if message.contains("already owns")
        ));
    }
}
