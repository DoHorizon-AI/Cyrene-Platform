//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 persistent_directory.rs                                         │
//! │  Module: cy_workspace_fabric::persistent_directory                 │
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

use crate::directory::{
    InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError, WorkspaceMembership,
};

const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    memberships: Vec<MembershipRecord>,
    descriptors: Vec<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
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
                version: 1,
                memberships: Vec::new(),
                descriptors: Vec::new(),
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
        let snapshot = Snapshot {
            version: 1,
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
        };
        let index = build_index(&snapshot)?;
        let bytes = serde_json::to_vec(&snapshot).map_err(storage_error)?;
        if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err(storage_error("directory snapshot exceeds 16 MiB"));
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| storage_error("directory state poisoned"))?;
        persist(&self.path, &bytes)?;
        *state = DirectoryState { snapshot, index };
        #[cfg(unix)]
        File::open(self.path.parent().expect("directory snapshot has a parent"))
            .and_then(|directory| directory.sync_all())
            .map_err(|_| {
                WorkspaceDirectoryError::Storage(
                    "directory revision published; durability not confirmed".into(),
                )
            })?;
        Ok(())
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

impl WorkspaceDirectory for FileWorkspaceDirectory {
    fn discover(
        &self,
        user: &UserIdentityRef,
        organization_id: &str,
        now_unix_ms: u64,
    ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
        let state = self
            .state
            .read()
            .map_err(|_| storage_error("directory state poisoned"))?;
        state.index.discover(user, organization_id, now_unix_ms)
    }

    fn is_member(&self, user: &UserIdentityRef, organization_id: &str, workspace_id: &str) -> bool {
        self.state
            .read()
            .is_ok_and(|state| state.index.is_member(user, organization_id, workspace_id))
    }
}

fn build_index(snapshot: &Snapshot) -> Result<InMemoryWorkspaceDirectory, WorkspaceDirectoryError> {
    if snapshot.version != 1 {
        return Err(storage_error("unsupported directory snapshot version"));
    }
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

    #[test]
    fn restart_preserves_authorized_discovery_only() {
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
        assert!(directory.is_member(&user(), "org-1", "workspace-1"));
        assert_eq!(
            directory.discover(&user(), "org-1", 1).unwrap(),
            vec![descriptor()]
        );
        let stranger = UserIdentityRef {
            subject: "user-2".into(),
            ..user()
        };
        assert!(directory
            .discover(&stranger, "org-1", 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_replacement_preserves_previous_revision() {
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
        assert!(directory.is_member(&user(), "org-1", "workspace-1"));
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
}
