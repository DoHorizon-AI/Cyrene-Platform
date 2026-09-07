//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 directory.rs                                                    │
//! │  Module: cy_artifact_transfer::directory                            │
//! │  Role: Verified portable directory materialization and publication.  │
//! │                                                                     │
//! │  模块职责：校验并原子发布 provider-neutral 的可移植目录。             │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Verified materialization for the provider-neutral portable directory contract.
//!
//! This module consumes an already authorized [`ArtifactRef`] and a caller
//! supplied blob source.  It does not select a source, interpret a locator, or
//! create an Artifact catalog.  The source only supplies raw bytes by digest;
//! this module verifies every byte before publishing a directory atomically.
//!
//! 本模块只负责消费已授权的 [`ArtifactRef`] 并校验、原子 materialize 目录。
//! 它不选择来源、不解释 locator，也不创建 Artifact catalog。来源边界按 digest
//! 提供 raw bytes，所有字节校验通过后才发布目录。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cy_manifest::{ArtifactRef, PortableDirectoryManifest};
use sha2::{Digest, Sha256};

use crate::TransferError;

static TEMP_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Read-only byte source selected by the Artifact Plane or an execution
/// adapter.
///
/// The implementation owns source selection and authorization.  It must
/// return the exact raw bytes identified by `digest`; filesystem-backed
/// implementations must reject symlinks instead of following them.  The
/// materializer never receives a source locator or a provider-private path.
pub trait ArtifactBlobSource {
    /// Open one raw CAS blob by its validated `sha256:` identity.
    fn open(&self, digest: &str) -> Result<Box<dyn Read>, TransferError>;
}

/// Result of a successful directory materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortableDirectoryMaterializeResult {
    /// Whether an existing, fully verified destination was reused.
    pub reused: bool,
    /// Number of files represented by the manifest.
    pub file_count: usize,
    /// Logical sum of raw file bytes, excluding manifest bytes.
    pub size_bytes: u64,
}

/// Verify and atomically materialize a V2 portable directory Artifact.
///
/// `manifest_bytes` may be a normal JSON representation.  Its typed content
/// is validated and its canonical JCS digest must equal both
/// `artifact.digest` and `artifact.manifest_digest`; the input representation
/// itself is not treated as a second identity scheme.  Each file is streamed
/// from `blobs`, verified for size and SHA-256, and written below a temporary
/// sibling directory.  The destination becomes visible only after every file
/// has passed validation.
///
/// This V1 publication path is Linux-only.  Callers must supply a destination
/// inside a trusted owner-controlled staging root and serialize external
/// writers; the symlink checks do not claim to eliminate races against an
/// arbitrary same-UID process mutating an unrelated directory.
/// The destination's immediate parent must already exist; callers own the
/// durability of any newly created ancestor directories.  After the
/// no-replace rename, a parent-directory fsync is attempted.  If that final
/// fsync fails, this function returns an I/O error even though the complete
/// destination is already visible; callers may retry and verify it.
///
/// 本 V1 发布路径仅支持 Linux。调用方必须把目标放在受信 owner 控制的 staging 根目录
/// 内并串行化外部写者；symlink 检查不宣称可以消除同 UID 进程修改任意目录时的竞态。
/// 目标的直接父目录必须预先存在；新建更高层 ancestor 的持久化由调用方负责。
/// no-replace rename 后会尝试同步父目录；若最后同步失败，函数仍返回 I/O 错误，
/// 但完整目标已经可见，调用方可以重试并重新校验。
pub fn materialize_portable_directory(
    artifact: &ArtifactRef,
    manifest_bytes: &[u8],
    blobs: &dyn ArtifactBlobSource,
    destination: &Path,
) -> Result<PortableDirectoryMaterializeResult, TransferError> {
    ensure_linux_materialization()?;
    let manifest = validate_artifact_and_manifest(artifact, manifest_bytes)?;
    validate_destination_path(destination)?;
    ensure_no_symlink_components(destination)?;

    if let Some(metadata) = symlink_metadata_if_exists(destination)? {
        if metadata.file_type().is_symlink() {
            return Err(contract("destination must not be a symlink"));
        }
        if !metadata.is_dir() {
            return Err(contract("destination exists but is not a directory"));
        }
        return verify_existing_directory(destination, &manifest, true);
    }

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_no_symlink_components(parent)?;
    let parent_metadata = symlink_metadata_if_exists(parent)?
        .ok_or_else(|| contract("destination parent directory must already exist"))?;
    if !parent_metadata.is_dir() {
        return Err(contract("destination parent exists but is not a directory"));
    }

    let temporary = create_temporary_directory(parent)?;
    let result = (|| {
        populate_directory(&temporary, &manifest, blobs)?;
        sync_directory_tree(&temporary)?;

        // Recheck immediately before publishing.  A competing publisher is
        // a conflict and must not cause an existing destination to be
        // overwritten by this operation.
        if let Some(metadata) = symlink_metadata_if_exists(destination)? {
            if metadata.file_type().is_symlink() {
                return Err(contract("destination must not be a symlink"));
            }
            if !metadata.is_dir() {
                return Err(contract("destination appeared as a non-directory"));
            }
            return verify_existing_directory(destination, &manifest, true);
        }

        // The no-replace rename is the only operation that publishes the
        // directory.  Linux's renameat2 closes the check-then-rename race;
        // other platforms fail closed instead of claiming the same guarantee.
        match publish_without_replacement(parent, &temporary, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = symlink_metadata_if_exists(destination)?
                    .ok_or_else(|| contract("destination disappeared during publication"))?;
                if metadata.file_type().is_symlink() {
                    return Err(contract("destination must not be a symlink"));
                }
                if !metadata.is_dir() {
                    return Err(contract("destination appeared as a non-directory"));
                }
                return verify_existing_directory(destination, &manifest, true);
            }
            Err(error) => return Err(error.into()),
        }
        // A post-rename parent sync makes the directory entry durable.  The
        // destination is already complete and atomically visible if this
        // final durability operation reports an I/O error.
        sync_directory(parent)?;
        Ok(PortableDirectoryMaterializeResult {
            reused: false,
            file_count: manifest.files.len(),
            size_bytes: manifest.size_bytes,
        })
    })();

    if result.is_err() || temporary.exists() {
        remove_temporary_directory(&temporary);
    }
    result
}

fn validate_artifact_and_manifest(
    artifact: &ArtifactRef,
    manifest_bytes: &[u8],
) -> Result<PortableDirectoryManifest, TransferError> {
    artifact.validate().map_err(TransferError::Contract)?;
    let manifest: PortableDirectoryManifest =
        serde_json::from_slice(manifest_bytes).map_err(|error| {
            contract(format!(
                "portable directory manifest JSON is invalid: {error}"
            ))
        })?;
    manifest
        .validate()
        .map_err(|error| contract(format!("portable directory manifest is invalid: {error}")))?;

    let manifest_digest = manifest.computed_digest();
    if artifact.digest != manifest_digest {
        return Err(TransferError::ArtifactDigest(format!(
            "Artifact digest {} does not match portable directory manifest {manifest_digest}",
            artifact.digest
        )));
    }
    if artifact.manifest_digest.as_deref() != Some(manifest_digest.as_str()) {
        return Err(TransferError::ArtifactDigest(
            "Artifact manifest_digest does not match the portable directory manifest".to_string(),
        ));
    }
    if artifact.size_bytes != manifest.size_bytes {
        return Err(TransferError::ArtifactDigest(format!(
            "Artifact size {} does not match portable directory logical size {}",
            artifact.size_bytes, manifest.size_bytes
        )));
    }
    Ok(manifest)
}

fn validate_destination_path(destination: &Path) -> Result<(), TransferError> {
    if destination.as_os_str().is_empty() || destination.file_name().is_none() {
        return Err(contract("destination must name a directory"));
    }
    if destination
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(contract(
            "destination must not contain a parent-directory component",
        ));
    }
    Ok(())
}

fn ensure_no_symlink_components(path: &Path) -> Result<(), TransferError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }

    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(contract(
                    "filesystem path must not contain a parent-directory component",
                ));
            }
            Component::Normal(part) => {
                current.push(part);
                let metadata = match fs::symlink_metadata(&current) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if metadata.file_type().is_symlink() {
                    return Err(contract(format!(
                        "filesystem path contains a symlink: {}",
                        current.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn symlink_metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>, TransferError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn create_temporary_directory(parent: &Path) -> Result<PathBuf, TransferError> {
    let process_id = std::process::id();
    for _ in 0..128 {
        let sequence = TEMP_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".cyrene-portable-directory-{process_id}-{sequence}"
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(contract(
        "could not reserve a unique temporary materialization directory",
    ))
}

fn populate_directory(
    temporary: &Path,
    manifest: &PortableDirectoryManifest,
    blobs: &dyn ArtifactBlobSource,
) -> Result<(), TransferError> {
    for entry in &manifest.files {
        let relative = Path::new(&entry.path);
        let output_path = temporary.join(relative);
        let parent = output_path
            .parent()
            .ok_or_else(|| contract("portable directory entry has no parent"))?;
        fs::create_dir_all(parent)?;
        ensure_no_symlink_components(parent)?;

        let mut input = blobs.open(&entry.digest)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        let (size_bytes, digest) = copy_and_digest(&mut input, &mut output, entry.size_bytes)?;
        output.sync_all()?;
        if size_bytes != entry.size_bytes {
            return Err(TransferError::ArtifactDigest(format!(
                "{} has size {size_bytes}, expected {}",
                entry.path, entry.size_bytes
            )));
        }
        if digest != entry.digest {
            return Err(TransferError::ArtifactDigest(format!(
                "{} has digest {digest}, expected {}",
                entry.path, entry.digest
            )));
        }
    }
    Ok(())
}

fn verify_existing_directory(
    destination: &Path,
    manifest: &PortableDirectoryManifest,
    reused: bool,
) -> Result<PortableDirectoryMaterializeResult, TransferError> {
    let expected = manifest
        .files
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    verify_existing_directory_entries(destination, Path::new(""), &expected, &mut seen)?;
    if seen.len() != expected.len() {
        return Err(contract(
            "existing destination is missing one or more portable directory files",
        ));
    }
    Ok(PortableDirectoryMaterializeResult {
        reused,
        file_count: manifest.files.len(),
        size_bytes: manifest.size_bytes,
    })
}

fn verify_existing_directory_entries(
    root: &Path,
    relative: &Path,
    expected: &BTreeMap<String, &cy_manifest::ArtifactDirectoryEntry>,
    seen: &mut BTreeSet<String>,
) -> Result<(), TransferError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(contract(format!(
                "existing destination contains a symlink: {}",
                path.display()
            )));
        }
        let child_relative = relative.join(entry.file_name());
        let child_name = child_relative
            .to_str()
            .ok_or_else(|| contract("existing destination contains a non-UTF-8 path component"))?;
        if metadata.is_dir() {
            let has_expected_descendant = expected
                .keys()
                .any(|candidate| candidate.starts_with(&format!("{child_name}/")));
            if !has_expected_descendant {
                return Err(contract(format!(
                    "existing destination contains an unexpected directory: {child_name}"
                )));
            }
            verify_existing_directory_entries(&path, &child_relative, expected, seen)?;
        } else if metadata.is_file() {
            let expected_entry = expected.get(child_name).ok_or_else(|| {
                contract(format!(
                    "existing destination contains an unexpected file: {child_name}"
                ))
            })?;
            verify_file(
                &path,
                expected_entry.digest.as_str(),
                expected_entry.size_bytes,
            )?;
            seen.insert(child_name.to_string());
        } else {
            return Err(contract(format!(
                "existing destination contains a non-file entry: {child_name}"
            )));
        }
    }
    Ok(())
}

fn verify_file(
    path: &Path,
    expected_digest: &str,
    expected_size: u64,
) -> Result<(), TransferError> {
    let mut input = File::open(path)?;
    let (size_bytes, digest) = copy_and_digest(&mut input, &mut std::io::sink(), expected_size)?;
    if size_bytes != expected_size {
        return Err(TransferError::ArtifactDigest(format!(
            "{} has size {size_bytes}, expected {expected_size}",
            path.display()
        )));
    }
    if digest != expected_digest {
        return Err(TransferError::ArtifactDigest(format!(
            "{} has digest {digest}, expected {expected_digest}",
            path.display()
        )));
    }
    Ok(())
}

fn copy_and_digest(
    input: &mut dyn Read,
    output: &mut dyn Write,
    expected_size: u64,
) -> Result<(u64, String), TransferError> {
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        // Read at most the remaining expected bytes plus one byte.  This
        // detects an oversized source promptly without buffering or writing a
        // complete unexpected model blob.
        let remaining = expected_size.saturating_sub(size_bytes);
        let read_limit = remaining
            .saturating_add(1)
            .min(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        let read_limit = usize::try_from(read_limit)
            .map_err(|_| contract("portable directory read limit exceeds usize"))?;
        let read = input.read(&mut buffer[..read_limit])?;
        if read == 0 {
            break;
        }
        let read_bytes = u64::try_from(read)
            .map_err(|_| contract("portable directory byte count exceeds u64"))?;
        if size_bytes
            .checked_add(read_bytes)
            .is_none_or(|total| total > expected_size)
        {
            return Err(TransferError::ArtifactDigest(format!(
                "blob exceeds expected size {expected_size} bytes"
            )));
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        size_bytes = size_bytes
            .checked_add(read_bytes)
            .ok_or_else(|| contract("portable directory byte count overflows u64"))?;
    }
    Ok((size_bytes, format!("sha256:{:x}", hasher.finalize())))
}

fn sync_directory(path: &Path) -> Result<(), TransferError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Sync every directory after its files, so rename publishes a complete tree.
///
/// 在 rename 前从叶到根同步每一级目录，确保发布的是完整目录树。
fn sync_directory_tree(path: &Path) -> Result<(), TransferError> {
    let mut child_directories = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)?;
        if metadata.file_type().is_symlink() {
            return Err(contract(format!(
                "temporary materialization contains a symlink: {}",
                child.display()
            )));
        }
        if metadata.is_dir() {
            child_directories.push(child);
        } else if !metadata.is_file() {
            return Err(contract(format!(
                "temporary materialization contains a non-file entry: {}",
                child.display()
            )));
        }
    }
    for child in child_directories {
        sync_directory_tree(&child)?;
    }
    sync_directory(path)
}

#[cfg(target_os = "linux")]
fn ensure_linux_materialization() -> Result<(), TransferError> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn ensure_linux_materialization() -> Result<(), TransferError> {
    Err(contract(
        "portable directory materialization requires Linux V1",
    ))
}

/// Publish a temporary sibling without replacing a competing destination.
///
/// Linux `renameat2(RENAME_NOREPLACE)` is used through rustix so the final
/// check and publish share one directory file descriptor.
fn publish_without_replacement(
    parent: &Path,
    temporary: &Path,
    destination: &Path,
) -> Result<(), std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        let parent_handle = File::open(parent)?;
        let temporary_name = temporary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "temporary directory has no basename",
            )
        })?;
        let destination_name = destination.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination has no basename",
            )
        })?;
        rustix::fs::renameat_with(
            &parent_handle,
            temporary_name,
            &parent_handle,
            destination_name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (parent, temporary, destination);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "portable directory no-replace publication requires Linux v1",
        ))
    }
}

fn remove_temporary_directory(path: &Path) {
    fs::remove_dir_all(path).ok();
}

fn contract(message: impl Into<String>) -> TransferError {
    TransferError::Contract(message.into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn no_replace_preserves_existing_empty_destination() {
        let temporary_root = tempfile::tempdir().expect("test temporary root");
        let parent = temporary_root.path();
        let temporary = parent.join(".complete-materialization");
        fs::create_dir(&temporary).expect("temporary directory");
        fs::create_dir(temporary.join("nested")).expect("nested temporary directory");
        fs::write(temporary.join("nested").join("weights.bin"), b"complete")
            .expect("complete temporary file");

        let destination = parent.join("destination");
        fs::create_dir(&destination).expect("empty destination directory");
        let before = fs::symlink_metadata(&destination).expect("destination metadata");

        let error = publish_without_replacement(parent, &temporary, &destination)
            .expect_err("existing destination must not be replaced");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

        let after = fs::symlink_metadata(&destination).expect("destination remains");
        assert_eq!(
            before.ino(),
            after.ino(),
            "destination inode must not change"
        );
        assert!(destination
            .read_dir()
            .expect("destination entries")
            .next()
            .is_none());
        assert_eq!(
            fs::read(temporary.join("nested").join("weights.bin")).expect("temporary remains"),
            b"complete"
        );
    }
}
