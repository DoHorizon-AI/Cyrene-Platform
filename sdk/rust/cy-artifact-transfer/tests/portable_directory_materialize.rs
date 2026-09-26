//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 portable_directory_materialize.rs                               │
//! │  Module: cy_artifact_transfer integration tests                     │
//! │  Role: Cross-SDK and failure-path tests for directory materialize.   │
//! │                                                                     │
//! │  模块职责：验证跨 SDK 目录 materialize 及失败清理边界。              │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Cross-language fixture and real-file tests for portable directory staging.
//!
//! The manifest fixture is emitted by the Python artifact SDK.  These tests
//! deliberately use a file-backed test source so the Rust consumer verifies
//! the same raw blobs that the Python provider publishes, without making the
//! Python provider's CAS layout part of the Rust API.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use cy_artifact_transfer::{
    materialize_portable_directory, ArtifactBlobSource, ArtifactKind, ArtifactRef,
    PortableDirectoryManifest, TransferError,
};
use tempfile::TempDir;

const PYTHON_MANIFEST: &[u8] = include_bytes!(
    "../../../../contracts/schemas/examples/portable_directory_manifest.example.json"
);
const EXPECTED_DIGEST: &str =
    "sha256:5d3c4bb7f4b864c409fa3baafb199f33f54c5fdfead4c6ec724f16898b2a828f";

struct FileBlobSource {
    root: PathBuf,
}

impl FileBlobSource {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl ArtifactBlobSource for FileBlobSource {
    fn open(&self, digest: &str) -> Result<Box<dyn Read>, TransferError> {
        let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
            TransferError::Contract("test source received an invalid digest".into())
        })?;
        let path = self.root.join(hex);
        let metadata = fs::symlink_metadata(&path).map_err(TransferError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(TransferError::Contract(
                "test source refuses to follow a symlink".into(),
            ));
        }
        Ok(Box::new(File::open(path).map_err(TransferError::Io)?))
    }
}

struct FailingSource;

impl ArtifactBlobSource for FailingSource {
    fn open(&self, _digest: &str) -> Result<Box<dyn Read>, TransferError> {
        Ok(Box::new(FailingReader { emitted: false }))
    }
}

struct FailingReader {
    emitted: bool,
}

impl Read for FailingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.emitted {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "injected source failure",
            ));
        }
        self.emitted = true;
        let bytes = b"pa";
        let count = bytes.len().min(buffer.len());
        buffer[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }
}

fn fixture_manifest_and_artifact() -> (PortableDirectoryManifest, ArtifactRef) {
    let manifest: PortableDirectoryManifest = serde_json::from_slice(PYTHON_MANIFEST).unwrap();
    let artifact = manifest
        .artifact_ref(ArtifactKind::new("producer.bundle").unwrap())
        .unwrap();
    assert_eq!(artifact.digest, EXPECTED_DIGEST);
    (manifest, artifact)
}

fn write_python_fixture_blobs(root: &Path) {
    let blobs = BTreeMap::from([
        (
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            b"abc".as_slice(),
        ),
        (
            "06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8",
            b"\x00\xff".as_slice(),
        ),
        (
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            b"".as_slice(),
        ),
    ]);
    fs::create_dir_all(root).unwrap();
    for (digest, bytes) in blobs {
        fs::write(root.join(digest), bytes).unwrap();
    }
}

fn python_fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/publish_portable_directory.py")
}

fn python_sdk_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../python/cyrene_artifacts/src")
}

#[test]
fn python_provider_publish_is_consumed_by_rust_materializer() {
    let temp = TempDir::new().unwrap();
    let exported = temp.path().join("python-export");
    let python = std::env::var_os("CYRENE_PYTHON").unwrap_or_else(|| "python3".into());
    let status = Command::new(python)
        .arg(python_fixture_script())
        .arg(&exported)
        .env("PYTHONPATH", python_sdk_source())
        .status()
        .expect("Python must be available for the cross-SDK acceptance test");
    assert!(status.success(), "Python publish adapter failed: {status}");

    let artifact: ArtifactRef =
        serde_json::from_slice(&fs::read(exported.join("artifact.json")).unwrap()).unwrap();
    let manifest_bytes = fs::read(exported.join("manifest.json")).unwrap();
    let destination = temp.path().join("rust-materialized");
    let source = FileBlobSource::new(&exported.join("blobs"));
    let result =
        materialize_portable_directory(&artifact, &manifest_bytes, &source, &destination).unwrap();

    assert!(!result.reused);
    assert_eq!(result.file_count, 3);
    assert_eq!(result.size_bytes, 5);
    assert_eq!(fs::read(destination.join("weights.bin")).unwrap(), b"abc");
    assert_eq!(
        fs::read(destination.join("z").join("é.txt")).unwrap(),
        b"\x00\xff"
    );
    assert_eq!(
        fs::read(destination.join("模型").join("空.txt")).unwrap(),
        b""
    );
}

#[test]
fn materializes_python_fixture_with_unicode_and_zero_byte_file() {
    let (_manifest, artifact) = fixture_manifest_and_artifact();
    let temp = TempDir::new().unwrap();
    let blob_root = temp.path().join("blobs");
    write_python_fixture_blobs(&blob_root);
    let destination = temp.path().join("materialized");
    let source = FileBlobSource::new(&blob_root);

    let result =
        materialize_portable_directory(&artifact, PYTHON_MANIFEST, &source, &destination).unwrap();
    assert!(!result.reused);
    assert_eq!(result.file_count, 3);
    assert_eq!(result.size_bytes, 5);
    assert_eq!(fs::read(destination.join("weights.bin")).unwrap(), b"abc");
    assert_eq!(
        fs::read(destination.join("z").join("é.txt")).unwrap(),
        b"\x00\xff"
    );
    assert_eq!(
        fs::read(destination.join("模型").join("空.txt")).unwrap(),
        b""
    );

    // A verified target is idempotent and does not require the source again.
    // 中文：目标已通过校验时，操作具有幂等性，且无需再次访问源目录。
    fs::remove_dir_all(&blob_root).unwrap();
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&blob_root),
        &destination,
    )
    .unwrap();
    assert!(result.reused);
}

#[test]
fn accepts_noncanonical_json_manifest_representation() {
    let (manifest, artifact) = fixture_manifest_and_artifact();
    let pretty_manifest = serde_json::to_vec_pretty(&manifest).unwrap();
    assert_ne!(pretty_manifest, manifest.canonical_bytes());
    let temp = TempDir::new().unwrap();
    let blob_root = temp.path().join("blobs");
    write_python_fixture_blobs(&blob_root);
    let destination = temp.path().join("materialized");

    let result = materialize_portable_directory(
        &artifact,
        &pretty_manifest,
        &FileBlobSource::new(&blob_root),
        &destination,
    )
    .unwrap();
    assert!(!result.reused);
    assert_eq!(fs::read(destination.join("weights.bin")).unwrap(), b"abc");
}

#[test]
fn corrupt_or_missing_blob_never_publishes_a_partial_directory() {
    let (_manifest, artifact) = fixture_manifest_and_artifact();
    let temp = TempDir::new().unwrap();
    let blob_root = temp.path().join("blobs");
    write_python_fixture_blobs(&blob_root);
    fs::write(
        blob_root.join("06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8"),
        b"wrong",
    )
    .unwrap();
    let destination = temp.path().join("materialized");
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&blob_root),
        &destination,
    );
    assert!(matches!(result, Err(TransferError::ArtifactDigest(_))));
    assert!(!destination.exists());
    assert!(!temp
        .path()
        .read_dir()
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".cyrene-portable-directory-")));

    let missing_blob =
        blob_root.join("06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8");
    fs::remove_file(missing_blob).unwrap();
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&blob_root),
        &destination,
    );
    assert!(result.is_err());
    assert!(!destination.exists());
}

#[test]
fn source_read_failure_and_existing_conflict_do_not_overwrite() {
    let (_manifest, artifact) = fixture_manifest_and_artifact();
    let temp = TempDir::new().unwrap();
    let destination = temp.path().join("materialized");
    let result =
        materialize_portable_directory(&artifact, PYTHON_MANIFEST, &FailingSource, &destination);
    assert!(matches!(result, Err(TransferError::Io(_))));
    assert!(!destination.exists());

    let blob_root = temp.path().join("blobs");
    write_python_fixture_blobs(&blob_root);
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("weights.bin"), b"old").unwrap();
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&blob_root),
        &destination,
    );
    assert!(matches!(result, Err(TransferError::ArtifactDigest(_))));
    assert_eq!(fs::read(destination.join("weights.bin")).unwrap(), b"old");
}

struct OversizedSource {
    bytes_read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ArtifactBlobSource for OversizedSource {
    fn open(&self, _digest: &str) -> Result<Box<dyn Read>, TransferError> {
        Ok(Box::new(OversizedReader {
            bytes_read: self.bytes_read.clone(),
        }))
    }
}

struct OversizedReader {
    bytes_read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Read for OversizedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.bytes_read
            .fetch_add(buffer.len(), std::sync::atomic::Ordering::Relaxed);
        buffer.fill(b'x');
        Ok(buffer.len())
    }
}

#[test]
fn oversized_blob_is_rejected_at_the_expected_size_boundary() {
    let (_manifest, artifact) = fixture_manifest_and_artifact();
    let temp = TempDir::new().unwrap();
    let bytes_read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &OversizedSource {
            bytes_read: bytes_read.clone(),
        },
        &temp.path().join("materialized"),
    );
    assert!(matches!(result, Err(TransferError::ArtifactDigest(_))));
    assert!(!temp.path().join("materialized").exists());
    assert_eq!(
        bytes_read.load(std::sync::atomic::Ordering::Relaxed),
        4,
        "the first 3-byte entry should read only its expected bytes plus one"
    );
}

#[cfg(unix)]
#[test]
fn target_and_source_symlinks_are_rejected() {
    let (_manifest, artifact) = fixture_manifest_and_artifact();
    let temp = TempDir::new().unwrap();
    let blob_root = temp.path().join("blobs");
    write_python_fixture_blobs(&blob_root);

    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let destination = temp.path().join("materialized-link");
    std::os::unix::fs::symlink(&outside, &destination).unwrap();
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&blob_root),
        &destination,
    );
    assert!(matches!(result, Err(TransferError::Contract(_))));
    assert!(outside.read_dir().unwrap().next().is_none());

    let symlink_blob_root = temp.path().join("symlink-blobs");
    fs::create_dir(&symlink_blob_root).unwrap();
    let outside_blob = temp.path().join("outside-blob");
    fs::write(&outside_blob, b"abc").unwrap();
    std::os::unix::fs::symlink(
        &outside_blob,
        symlink_blob_root.join("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
    )
    .unwrap();
    let result = materialize_portable_directory(
        &artifact,
        PYTHON_MANIFEST,
        &FileBlobSource::new(&symlink_blob_root),
        &temp.path().join("source-link-output"),
    );
    assert!(matches!(result, Err(TransferError::Contract(_))));
}

#[test]
fn manifest_identity_must_match_authorized_artifact() {
    let (manifest, mut artifact) = fixture_manifest_and_artifact();
    artifact.manifest_digest = None;
    let temp = TempDir::new().unwrap();
    let result = materialize_portable_directory(
        &artifact,
        &manifest.canonical_bytes(),
        &FailingSource,
        &temp.path().join("materialized"),
    );
    assert!(matches!(result, Err(TransferError::ArtifactDigest(_))));
}
