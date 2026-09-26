//! Durable session correlation storage. Resource/Lease authority is not stored here.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::DispatchError;

pub trait ExecutionSessionStore: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, DispatchError>;
    fn save(&self, snapshot: &[u8]) -> Result<(), DispatchError>;
}

/// One service instance owns this directory. Credentials are kept in a private
/// snapshot, never in the execution event/intent ledger. A persistent volume is
/// required; do not point different replicas at independent copies of this file.
pub struct FileExecutionSessionStore {
    path: PathBuf,
    _ownership: File,
    gate: Mutex<()>,
}

impl FileExecutionSessionStore {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, DispatchError> {
        let directory = directory.into();
        if !directory.exists() {
            fs::create_dir_all(&directory).map_err(storage_error)?;
            #[cfg(unix)]
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(storage_error)?;
        }
        let metadata = fs::symlink_metadata(&directory).map_err(storage_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(DispatchError::input(
                "SESSION_DIRECTORY_INVALID",
                "session directory must be a real directory",
            ));
        }
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(DispatchError::input(
                "SESSION_DIRECTORY_PERMISSIONS",
                "session directory must be private (0700)",
            ));
        }
        let lock_path = directory.join("owner.lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err(DispatchError::input(
                "SESSION_LOCK_INVALID",
                "session lock must be a regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let ownership = options.open(lock_path).map_err(storage_error)?;
        ownership.try_lock().map_err(|_| {
            DispatchError::input(
                "SESSION_STORE_BUSY",
                "another control service owns the session store",
            )
        })?;
        Ok(Self {
            path: directory.join("sessions.json"),
            _ownership: ownership,
            gate: Mutex::new(()),
        })
    }
}

impl ExecutionSessionStore for FileExecutionSessionStore {
    fn load(&self) -> Result<Option<Vec<u8>>, DispatchError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| storage_error("session store poisoned"))?;
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_error(error)),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 64 * 1024 * 1024
        {
            return Err(storage_error(
                "session snapshot must be a regular file under 64 MiB",
            ));
        }
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(storage_error("session snapshot must be private (0600)"));
        }
        fs::read(&self.path).map(Some).map_err(storage_error)
    }

    fn save(&self, snapshot: &[u8]) -> Result<(), DispatchError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| storage_error("session store poisoned"))?;
        if snapshot.len() > 64 * 1024 * 1024 {
            return Err(storage_error("session snapshot exceeds 64 MiB"));
        }
        let parent = self.path.parent().expect("session directory");
        let temporary = parent.join(format!("sessions-{}.pending", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), std::io::Error> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(snapshot)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(storage_error)
    }
}

fn storage_error(_error: impl std::fmt::Display) -> DispatchError {
    // Never include serialized grants or resume credentials in errors.
    DispatchError::unknown("execution session persistence failed; reconcile before dispatch")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_preserves_snapshot_and_rejects_competing_owner() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("sessions");
        let first = FileExecutionSessionStore::open(&directory).unwrap();
        assert!(first.load().unwrap().is_none());
        first.save(br#"{"version":1}"#).unwrap();
        assert!(FileExecutionSessionStore::open(&directory).is_err());
        drop(first);
        let second = FileExecutionSessionStore::open(&directory).unwrap();
        assert_eq!(second.load().unwrap().unwrap(), br#"{"version":1}"#);
    }
}
