//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 acquisition.rs                                                  │
//! │  Module: cy_artifact_transfer::acquisition                          │
//! │  Role: External-source to immutable-Artifact provider seam.         │
//! │                                                                     │
//! │  模块职责：把外部可变来源导入为内部不可变 Artifact 的 Provider seam。    │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

use cy_kernel_contract::Identity;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactKind, ArtifactRef, TransferError};

/// Provider-specific mutable source request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSource {
    pub provider: String,
    pub locator: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision: Option<String>,
}

/// Generic import Operation associated with one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceImportJob {
    pub operation: Identity,
    pub source: ExternalSource,
}

/// Immutable result of importing an external source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub source: ExternalSource,
    pub artifact: ArtifactRef,
}

/// Replaceable GitHub, Hugging Face, mirror, or cloud acquisition boundary.
pub trait AcquisitionProvider: Send + Sync {
    fn import(&self, job: &SourceImportJob) -> Result<SourceSnapshot, TransferError>;
}

/// Reference HTTP/HF-like acquisition provider that atomically publishes internally.
#[derive(Clone)]
pub struct HttpAcquisitionProvider {
    client: Client,
    artifact_root: PathBuf,
}

impl HttpAcquisitionProvider {
    pub fn new(artifact_root: PathBuf) -> Result<Self, TransferError> {
        if !artifact_root.is_absolute() {
            return Err(TransferError::Contract(
                "acquisition Artifact root must be absolute".to_string(),
            ));
        }
        fs::create_dir_all(&artifact_root)?;
        Ok(Self {
            client: Client::new(),
            artifact_root,
        })
    }

    fn publish_reader(
        &self,
        job: &SourceImportJob,
        mut input: impl Read,
    ) -> Result<SourceSnapshot, TransferError> {
        let operation_hash = format!("{:x}", Sha256::digest(job.operation.id.as_bytes()));
        let temporary = self
            .artifact_root
            .join(format!(".source-import-{operation_hash}"));
        let result = (|| {
            let mut output = File::create(&temporary)?;
            let mut hasher = Sha256::new();
            let mut size_bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = input
                    .read(&mut buffer)
                    .map_err(|error| TransferError::Http(error.to_string()))?;
                if read == 0 {
                    break;
                }
                output.write_all(&buffer[..read])?;
                hasher.update(&buffer[..read]);
                size_bytes = size_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            }
            if size_bytes == 0 {
                return Err(TransferError::ArtifactDigest(
                    "external source produced an empty Artifact".to_string(),
                ));
            }
            output.sync_all()?;
            let digest_hex = format!("{:x}", hasher.finalize());
            let destination = self.artifact_root.join(&digest_hex);
            if destination.exists() {
                let (existing_digest, existing_size) = digest_file(&destination)?;
                if existing_digest != digest_hex || existing_size != size_bytes {
                    return Err(TransferError::ArtifactDigest(
                        "existing acquired Artifact is corrupt".to_string(),
                    ));
                }
                fs::remove_file(&temporary)?;
            } else {
                fs::rename(&temporary, &destination)?;
            }
            Ok(SourceSnapshot {
                source: job.source.clone(),
                artifact: ArtifactRef {
                    uri: format!("artifact://sha256/{digest_hex}"),
                    digest: format!("sha256:{digest_hex}"),
                    size_bytes,
                    kind: ArtifactKind::generic(),
                    manifest_digest: None,
                },
            })
        })();
        if result.is_err() {
            fs::remove_file(&temporary).ok();
        }
        result
    }
}

fn digest_file(path: &std::path::Path) -> Result<(String, u64), TransferError> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

impl AcquisitionProvider for HttpAcquisitionProvider {
    fn import(&self, job: &SourceImportJob) -> Result<SourceSnapshot, TransferError> {
        job.operation.validate().map_err(|error| {
            TransferError::Contract(format!("{}: {}", error.reason_code, error.message))
        })?;
        let fixture_http =
            job.source.provider == "fixture-http" && job.source.locator.starts_with("http://");
        if job.source.provider.is_empty()
            || (!job.source.locator.starts_with("https://") && !fixture_http)
        {
            return Err(TransferError::Contract(
                "external acquisition requires HTTPS; plain HTTP is fixture-only".to_string(),
            ));
        }
        let response = self
            .client
            .get(&job.source.locator)
            .send()
            .map_err(|error| TransferError::Http(error.to_string()))?
            .error_for_status()
            .map_err(|error| TransferError::Http(error.to_string()))?;
        self.publish_reader(job, response)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn fixture_http_source_becomes_internal_content_addressed_artifact() {
        let root = tempfile::tempdir().unwrap();
        let provider = HttpAcquisitionProvider::new(root.path().to_path_buf()).unwrap();
        let snapshot = provider
            .publish_reader(
                &SourceImportJob {
                    operation: Identity {
                        id: "source-import-1".to_string(),
                        generation: 1,
                    },
                    source: ExternalSource {
                        provider: "hf-like-fixture".to_string(),
                        locator: "https://hf.fixture/snapshot".to_string(),
                        revision: Some("commit-1".to_string()),
                    },
                },
                Cursor::new(b"fixture-snapshot"),
            )
            .unwrap();
        assert!(snapshot.artifact.uri.starts_with("artifact://sha256/"));
        assert!(root.path().join(&snapshot.artifact.digest[7..]).is_file());
    }
}
