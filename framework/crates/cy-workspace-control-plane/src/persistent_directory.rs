//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 persistent_directory.rs                                         │
//! │  Module: cy_workspace_control_plane::persistent_directory                 │
//! │  Role: Durable membership and connection descriptor storage.        │
//! │                                                                     │
//! │  模块职责：持久保存成员关系与连接描述符。                                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::device_registry::{
    ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus, WorkspaceDeviceKey,
    WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
};
use crate::directory::{
    InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError, WorkspaceMembership,
};

const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
const CURRENT_SNAPSHOT_VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    memberships: Vec<MembershipRecord>,
    descriptors: Vec<Vec<u8>>,
    #[serde(default)]
    devices: Vec<WorkspaceDeviceRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipRecord {
    issuer: String,
    subject: String,
    organization_id: String,
    workspace_id: String,
    roles: BTreeSet<String>,
}

struct DirectoryState {
    snapshot: Snapshot,
    index: InMemoryWorkspaceDirectory,
}

/// File-backed Directory for one owner on a persistent volume.
///
/// The snapshot is replaced atomically. Opening an invalid snapshot fails closed
/// and leaves its original bytes intact for operator recovery.
///
/// 单实例持有持久卷；无效快照拒绝加载并保留原文件。
pub struct FileWorkspaceDirectory {
    path: PathBuf,
    _ownership: File,
    state: RwLock<DirectoryState>,
}

impl FileWorkspaceDirectory {
    /// Open or initialize a private Directory path and hold its exclusive lock.
    ///
    /// Returns a storage error if another owner is active or the snapshot is invalid.
    /// 打开私有目录并持有独占锁；若已有实例或快照无效则失败。
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, WorkspaceDirectoryError> {
        let directory = directory.into();
        if !directory.exists() {
            fs::create_dir_all(&directory).map_err(storage_error)?;
            #[cfg(unix)]
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(storage_error)?;
        }
        verify_private_directory(&directory)?;
        let lock_path = directory.join("owner.lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(storage_error("invalid owner lock"));
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let ownership = options.open(lock_path).map_err(storage_error)?;
        #[cfg(unix)]
        if ownership
            .metadata()
            .map_err(storage_error)?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(storage_error("owner lock must be private"));
        }
        ownership
            .try_lock()
            .map_err(|_| storage_error("directory already has an owner"))?;

        let path = directory.join("directory.json");
        let snapshot = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > MAX_SNAPSHOT_BYTES
                {
                    return Err(storage_error("invalid directory snapshot"));
                }
                #[cfg(unix)]
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(storage_error("directory snapshot must be private"));
                }
                let bytes = fs::read(&path).map_err(storage_error)?;
                serde_json::from_slice(&bytes)
                    .map_err(|_| storage_error("invalid directory snapshot"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Snapshot {
                version: CURRENT_SNAPSHOT_VERSION,
                memberships: Vec::new(),
                descriptors: Vec::new(),
                devices: Vec::new(),
            },
            Err(error) => return Err(storage_error(error)),
        };
        let index = build_index(&snapshot)?;
        Ok(Self {
            path,
            _ownership: ownership,
            state: RwLock::new(DirectoryState { snapshot, index }),
        })
    }

    /// Publish one complete Directory revision after validating every record.
    /// Validation and pre-rename write errors retain the previous revision.
    /// 校验后发布完整版本；校验及替换前写入失败时保留旧版本。
    pub fn replace(
        &self,
        memberships: Vec<WorkspaceMembership>,
        descriptors: Vec<WorkspaceConnectionDescriptor>,
    ) -> Result<(), WorkspaceDirectoryError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| storage_error("directory state poisoned"))?;
        let snapshot = Snapshot {
            version: CURRENT_SNAPSHOT_VERSION,
            memberships: memberships
                .into_iter()
                .map(|membership| MembershipRecord {
                    issuer: membership.user.issuer,
                    subject: membership.user.subject,
                    organization_id: membership.organization_id,
                    workspace_id: membership.workspace_id,
                    roles: membership.roles,
                })
                .collect(),
            descriptors: descriptors
                .into_iter()
                .map(|descriptor| descriptor.encode_to_vec())
                .collect(),
            devices: state.snapshot.devices.clone(),
        };
        publish_snapshot(&self.path, &mut state, snapshot)
    }

    /// Return the count of currently published memberships.
    /// 返回当前已发布的成员关系数量。
    pub fn membership_count(&self) -> Result<usize, WorkspaceDirectoryError> {
        self.state
            .read()
            .map(|state| state.snapshot.memberships.len())
            .map_err(|_| storage_error("directory state poisoned"))
    }
}

#[tonic::async_trait]
impl WorkspaceDirectory for FileWorkspaceDirectory {
    async fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        let state = self
            .state
            .read()
            .map_err(|_| storage_error("directory state poisoned"))?;
        state
            .index
            .discover_sync(user, organization_id, now_unix_ms)
    }

    async fn is_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<bool, WorkspaceDirectoryError> {
        let state = self
            .state
            .read()
            .map_err(|_| storage_error("directory state poisoned"))?;
        state
            .index
            .is_member_sync(user, organization_id, workspace_id)
    }

    async fn roles_for_member(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        workspace_id: &str,
    ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
        let state = self
            .state
            .read()
            .map_err(|_| storage_error("directory state poisoned"))?;
        state
            .index
            .roles_for_member_sync(user, organization_id, workspace_id)
    }
}

impl WorkspaceDeviceRegistry for FileWorkspaceDirectory {
    fn import_approved_device_certificate(
        &self,
        certificate: ApprovedWorkspaceDeviceCertificate,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
        let record = WorkspaceDeviceRecord {
            key: certificate.key,
            certificate_fingerprint_sha256: certificate.certificate_fingerprint_sha256,
            authorization_status: DeviceAuthorizationStatus::Approved,
        };
        validate_device_records(std::slice::from_ref(&record))?;

        let mut state = self
            .state
            .write()
            .map_err(|_| storage_error("directory state poisoned"))?;
        if state
            .snapshot
            .devices
            .iter()
            .any(|stored| stored.key == record.key)
        {
            return Err(WorkspaceDirectoryError::Identity(
                "Workspace device record already exists".to_string(),
            ));
        }
        if state.snapshot.devices.iter().any(|stored| {
            stored.certificate_fingerprint_sha256 == record.certificate_fingerprint_sha256
        }) {
            return Err(WorkspaceDirectoryError::Identity(
                "device certificate fingerprint is already recorded".to_string(),
            ));
        }

        let mut snapshot = state.snapshot.clone();
        snapshot.version = CURRENT_SNAPSHOT_VERSION;
        snapshot.devices.push(record.clone());
        publish_snapshot(&self.path, &mut state, snapshot)?;
        Ok(record)
    }

    fn revoke_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| storage_error("directory state poisoned"))?;
        let mut snapshot = state.snapshot.clone();
        let record = snapshot
            .devices
            .iter_mut()
            .find(|record| &record.key == key)
            .ok_or_else(|| {
                WorkspaceDirectoryError::Identity(
                    "Workspace device record was not found".to_string(),
                )
            })?;
        if record.authorization_status == DeviceAuthorizationStatus::Revoked {
            return Ok(record.clone());
        }
        if record.authorization_status != DeviceAuthorizationStatus::Approved {
            return Err(WorkspaceDirectoryError::Identity(
                "invalid Workspace device authorization transition".to_string(),
            ));
        }
        record.authorization_status = DeviceAuthorizationStatus::Revoked;
        let updated = record.clone();
        snapshot.version = CURRENT_SNAPSHOT_VERSION;
        publish_snapshot(&self.path, &mut state, snapshot)?;
        Ok(updated)
    }

    fn find_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
        self.state
            .read()
            .map(|state| {
                state
                    .snapshot
                    .devices
                    .iter()
                    .find(|record| &record.key == key)
                    .cloned()
            })
            .map_err(|_| storage_error("directory state poisoned"))
    }

    fn find_device_by_certificate_fingerprint(
        &self,
        fingerprint_sha256: &str,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError> {
        if !is_sha256_fingerprint(fingerprint_sha256) {
            return Err(WorkspaceDirectoryError::Identity(
                "device certificate fingerprint must be 64 lowercase SHA-256 hex characters"
                    .to_string(),
            ));
        }
        self.state
            .read()
            .map(|state| {
                state
                    .snapshot
                    .devices
                    .iter()
                    .find(|record| record.certificate_fingerprint_sha256 == fingerprint_sha256)
                    .cloned()
            })
            .map_err(|_| storage_error("directory state poisoned"))
    }
}

fn build_index(snapshot: &Snapshot) -> Result<InMemoryWorkspaceDirectory, WorkspaceDirectoryError> {
    if !matches!(snapshot.version, 1 | CURRENT_SNAPSHOT_VERSION) {
        return Err(storage_error("unsupported directory snapshot version"));
    }
    validate_device_records(&snapshot.devices)?;
    let mut memberships = Vec::with_capacity(snapshot.memberships.len());
    let mut seen_memberships = BTreeSet::new();
    for record in &snapshot.memberships {
        if record.issuer.is_empty()
            || record.subject.is_empty()
            || record.organization_id.is_empty()
            || record.workspace_id.is_empty()
            || record.roles.is_empty()
            || record.roles.iter().any(String::is_empty)
        {
            return Err(WorkspaceDirectoryError::Identity(
                "membership identity, scope, and roles are required".to_string(),
            ));
        }
        if !seen_memberships.insert((
            &record.issuer,
            &record.subject,
            &record.organization_id,
            &record.workspace_id,
        )) {
            return Err(WorkspaceDirectoryError::Identity(
                "duplicate Workspace membership".to_string(),
            ));
        }
        memberships.push(WorkspaceMembership {
            user: UserIdentityRef {
                issuer: record.issuer.clone(),
                subject: record.subject.clone(),
            },
            organization_id: record.organization_id.clone(),
            workspace_id: record.workspace_id.clone(),
            roles: record.roles.clone(),
        });
    }
    let mut descriptors = Vec::with_capacity(snapshot.descriptors.len());
    let mut seen_descriptors = BTreeSet::new();
    for bytes in &snapshot.descriptors {
        let descriptor = WorkspaceConnectionDescriptor::decode(bytes.as_slice()).map_err(|_| {
            WorkspaceDirectoryError::Descriptor("invalid descriptor bytes".to_string())
        })?;
        if !seen_descriptors.insert((
            descriptor.organization_id.clone(),
            descriptor.workspace_id.clone(),
        )) {
            return Err(WorkspaceDirectoryError::Descriptor(
                "duplicate Workspace descriptor".to_string(),
            ));
        }
        descriptors.push(descriptor);
    }
    InMemoryWorkspaceDirectory::new(memberships, descriptors)
}

/// Validate persisted device identity and certificate-fingerprint uniqueness.
///
/// The complete directory snapshot is rejected on ambiguity so a fingerprint
/// cannot silently identify more than one device.
///
/// 校验持久设备记录唯一性，避免一个证书指纹映射到多个设备。
fn validate_device_records(
    records: &[WorkspaceDeviceRecord],
) -> Result<(), WorkspaceDirectoryError> {
    let mut seen_keys = BTreeSet::new();
    let mut seen_fingerprints = BTreeSet::new();
    for record in records {
        if record.key.organization_id.is_empty()
            || record.key.workspace_id.is_empty()
            || record.key.device_id.is_empty()
        {
            return Err(WorkspaceDirectoryError::Identity(
                "device organization, Workspace, and device IDs are required".to_string(),
            ));
        }
        if !is_sha256_fingerprint(&record.certificate_fingerprint_sha256) {
            return Err(WorkspaceDirectoryError::Identity(
                "device certificate fingerprint must be 64 lowercase SHA-256 hex characters"
                    .to_string(),
            ));
        }
        if !seen_keys.insert(&record.key) {
            return Err(WorkspaceDirectoryError::Identity(
                "duplicate Workspace device identity".to_string(),
            ));
        }
        if !seen_fingerprints.insert(&record.certificate_fingerprint_sha256) {
            return Err(WorkspaceDirectoryError::Identity(
                "duplicate device certificate fingerprint".to_string(),
            ));
        }
    }
    Ok(())
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Atomically persist and publish a validated complete Directory revision.
///
/// The caller holds the state write lock so concurrent membership and device
/// updates cannot overwrite one another.
///
/// 调用方持有写锁，保证成员与设备的并发更新不会互相覆盖。
fn publish_snapshot(
    path: &Path,
    state: &mut DirectoryState,
    snapshot: Snapshot,
) -> Result<(), WorkspaceDirectoryError> {
    let index = build_index(&snapshot)?;
    let bytes = serde_json::to_vec(&snapshot).map_err(storage_error)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(storage_error("directory snapshot exceeds 16 MiB"));
    }
    persist(path, &bytes)?;
    *state = DirectoryState { snapshot, index };
    #[cfg(unix)]
    File::open(path.parent().expect("directory snapshot has a parent"))
        .and_then(|directory| directory.sync_all())
        .map_err(|_| {
            WorkspaceDirectoryError::Storage(
                "directory revision published; durability not confirmed".into(),
            )
        })?;
    Ok(())
}

fn verify_private_directory(directory: &Path) -> Result<(), WorkspaceDirectoryError> {
    let metadata = fs::symlink_metadata(directory).map_err(storage_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(storage_error("directory path must be a real directory"));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(storage_error("directory path must be private"));
    }
    Ok(())
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceDirectoryError> {
    let parent = path.parent().expect("directory snapshot has a parent");
    let temporary = parent.join(format!("directory-{}.pending", uuid::Uuid::new_v4()));
    let result = (|| -> Result<(), std::io::Error> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(storage_error)
}

fn storage_error(_error: impl std::fmt::Display) -> WorkspaceDirectoryError {
    WorkspaceDirectoryError::Storage("directory persistence failed; original state retained".into())
}

#[cfg(test)]
mod tests {
    use cy_proto::core_v1::ConnectivityMode;
    use cy_proto::workspace_v1::WorkspaceConnectionCandidate;
    use tempfile::TempDir;

    use super::*;

    fn user() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://issuer.test".into(),
            subject: "user-1".into(),
        }
    }

    fn membership() -> WorkspaceMembership {
        WorkspaceMembership {
            user: user(),
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            roles: BTreeSet::from(["member".into()]),
        }
    }

    fn descriptor() -> WorkspaceConnectionDescriptor {
        WorkspaceConnectionDescriptor {
            descriptor_version: "cyrene.workspace.connection.v1".into(),
            workspace_id: "workspace-1".into(),
            organization_id: "org-1".into(),
            display_name: "Workspace One".into(),
            candidates: vec![WorkspaceConnectionCandidate {
                mode: ConnectivityMode::LanDirect as i32,
                provider_id: "lan".into(),
                connection_uri: "https://workspace.test".into(),
                server_name: "workspace.test".into(),
                priority: 0,
                routing_hint: Vec::new(),
            }],
            expires_at: Some(prost_types::Timestamp {
                seconds: 100,
                nanos: 0,
            }),
        }
    }

    fn device_key(device_id: &str) -> WorkspaceDeviceKey {
        WorkspaceDeviceKey {
            organization_id: "org-1".into(),
            workspace_id: "workspace-1".into(),
            device_id: device_id.into(),
        }
    }

    fn approved_certificate(
        device_id: &str,
        fingerprint: char,
    ) -> ApprovedWorkspaceDeviceCertificate {
        ApprovedWorkspaceDeviceCertificate {
            key: device_key(device_id),
            certificate_fingerprint_sha256: std::iter::repeat_n(fingerprint, 64).collect(),
        }
    }

    #[tokio::test]
    async fn restart_preserves_authorized_discovery_only() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("directory");
        {
            let directory = FileWorkspaceDirectory::open(&path).unwrap();
            directory
                .replace(vec![membership()], vec![descriptor()])
                .unwrap();
            assert_eq!(directory.membership_count().unwrap(), 1);
            assert!(FileWorkspaceDirectory::open(&path).is_err());
        }
        let directory = FileWorkspaceDirectory::open(&path).unwrap();
        assert!(directory
            .is_member(&user(), "org-1", "workspace-1")
            .await
            .unwrap());
        assert_eq!(
            directory.discover(&user(), "org-1", 1).await.unwrap(),
            vec![descriptor()]
        );
        let stranger = UserIdentityRef {
            subject: "user-2".into(),
            ..user()
        };
        assert!(directory
            .discover(&stranger, "org-1", 1)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn invalid_replacement_preserves_previous_revision() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("directory");
        let directory = FileWorkspaceDirectory::open(&path).unwrap();
        directory
            .replace(vec![membership()], vec![descriptor()])
            .unwrap();
        let before = fs::read(path.join("directory.json")).unwrap();
        assert!(directory
            .replace(vec![membership(), membership()], vec![descriptor()])
            .is_err());
        assert_eq!(fs::read(path.join("directory.json")).unwrap(), before);
        assert!(directory
            .is_member(&user(), "org-1", "workspace-1")
            .await
            .unwrap());
    }

    #[test]
    fn corrupted_snapshot_fails_closed_without_replacing_file() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("directory");
        {
            let directory = FileWorkspaceDirectory::open(&path).unwrap();
            directory
                .replace(vec![membership()], vec![descriptor()])
                .unwrap();
        }
        let snapshot = path.join("directory.json");
        fs::write(&snapshot, b"{bad json").unwrap();
        assert!(FileWorkspaceDirectory::open(&path).is_err());
        assert_eq!(fs::read(&snapshot).unwrap(), b"{bad json");
    }

    #[tokio::test]
    async fn approved_device_certificate_and_revocation_survive_restart_and_directory_replace() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("directory");
        let expected;
        {
            let directory = FileWorkspaceDirectory::open(&path).unwrap();
            let enrolled = directory
                .import_approved_device_certificate(approved_certificate("device-1", 'a'))
                .unwrap();
            assert_eq!(
                enrolled.authorization_status,
                DeviceAuthorizationStatus::Approved
            );
            expected = directory.revoke_device(&enrolled.key).unwrap();

            directory
                .replace(vec![membership()], vec![descriptor()])
                .unwrap();
        }

        let directory = FileWorkspaceDirectory::open(&path).unwrap();
        assert_eq!(
            directory.find_device(&device_key("device-1")).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            directory
                .find_device_by_certificate_fingerprint(&expected.certificate_fingerprint_sha256)
                .unwrap(),
            Some(expected)
        );
        assert!(directory
            .is_member(&user(), "org-1", "workspace-1")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn poisoned_snapshot_reads_return_storage_errors_not_empty_authorization() {
        let root = TempDir::new().unwrap();
        let directory = FileWorkspaceDirectory::open(root.path().join("directory")).unwrap();
        let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = directory.state.write().unwrap();
            panic!("poison Directory state for fail-closed test");
        }));
        assert!(poison_result.is_err());

        assert!(matches!(
            directory.is_member(&user(), "org-1", "workspace-1").await,
            Err(WorkspaceDirectoryError::Storage(_))
        ));
        assert!(matches!(
            directory
                .roles_for_member(&user(), "org-1", "workspace-1")
                .await,
            Err(WorkspaceDirectoryError::Storage(_))
        ));
        assert!(matches!(
            directory.discover(&user(), "org-1", 1).await,
            Err(WorkspaceDirectoryError::Storage(_))
        ));
    }

    #[test]
    fn approved_device_import_rejects_duplicate_identity_fingerprint_and_malformed_digest() {
        let root = TempDir::new().unwrap();
        let directory = FileWorkspaceDirectory::open(root.path().join("directory")).unwrap();
        directory
            .import_approved_device_certificate(approved_certificate("device-1", 'a'))
            .unwrap();

        assert!(directory
            .import_approved_device_certificate(approved_certificate("device-1", 'b'))
            .is_err());
        assert!(directory
            .import_approved_device_certificate(approved_certificate("device-2", 'a'))
            .is_err());
        assert!(directory
            .import_approved_device_certificate(ApprovedWorkspaceDeviceCertificate {
                key: device_key("device-3"),
                certificate_fingerprint_sha256: "A".repeat(64),
            })
            .is_err());
        assert!(directory
            .find_device(&device_key("device-2"))
            .unwrap()
            .is_none());
        assert!(directory
            .find_device_by_certificate_fingerprint("not-a-fingerprint")
            .is_err());
    }

    #[test]
    fn approved_device_revocation_is_idempotent() {
        let root = TempDir::new().unwrap();
        let directory = FileWorkspaceDirectory::open(root.path().join("directory")).unwrap();
        let enrolled = directory
            .import_approved_device_certificate(approved_certificate("device-1", 'a'))
            .unwrap();
        let revoked = directory.revoke_device(&enrolled.key).unwrap();
        assert_eq!(
            revoked.authorization_status,
            DeviceAuthorizationStatus::Revoked
        );
        assert_eq!(directory.revoke_device(&revoked.key).unwrap(), revoked);
        assert!(directory
            .import_approved_device_certificate(approved_certificate("device-1", 'a'))
            .is_err());
    }

    #[test]
    fn version_one_directory_snapshot_loads_and_upgrades_on_device_write() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("directory");
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot_path = path.join("directory.json");
        fs::write(
            &snapshot_path,
            br#"{"version":1,"memberships":[],"descriptors":[]}"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&snapshot_path, fs::Permissions::from_mode(0o600)).unwrap();

        let directory = FileWorkspaceDirectory::open(&path).unwrap();
        directory
            .import_approved_device_certificate(approved_certificate("device-1", 'a'))
            .unwrap();
        drop(directory);

        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(snapshot_path).unwrap()).unwrap();
        assert_eq!(stored["version"], CURRENT_SNAPSHOT_VERSION);
        assert_eq!(stored["devices"].as_array().unwrap().len(), 1);
        assert_eq!(
            FileWorkspaceDirectory::open(&path)
                .unwrap()
                .find_device(&device_key("device-1"))
                .unwrap()
                .unwrap()
                .authorization_status,
            DeviceAuthorizationStatus::Approved
        );
    }
}
