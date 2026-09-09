//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 http.rs                                                         │
//! │  Module: cy_artifact_transfer::http                                 │
//! │  Role: HTTPS Range download, verification, resume, atomic publish.  │
//! │                                                                     │
//! │  模块职责：执行 Range 下载、part/full 校验、断点恢复和原子发布。          │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, RANGE};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{TransferCheckpoint, TransferPart, TransferSession};

/// Transfer failure with integrity and publication errors separated.
#[derive(Debug, Error)]
pub enum TransferError {
    #[error("TRANSFER_CONTRACT_INVALID: {0}")]
    Contract(String),
    #[error("TRANSFER_HTTP_FAILED: {0}")]
    Http(String),
    #[error("TRANSFER_IO_FAILED: {0}")]
    Io(#[from] std::io::Error),
    #[error("TRANSFER_CHECKPOINT_INVALID: {0}")]
    Checkpoint(String),
    #[error("TRANSFER_PART_DIGEST_MISMATCH: {0}")]
    PartDigest(String),
    #[error("ARTIFACT_DIGEST_MISMATCH: {0}")]
    ArtifactDigest(String),
    #[error("TRANSFER_AUTHORIZATION_FAILED: {0}")]
    Authorization(String),
    #[error("TRANSFER_POLICY_DENIED: {0}")]
    Policy(String),
}

/// Exact resume evidence returned by a successful transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferResult {
    pub downloaded_parts: usize,
    pub reused_parts: usize,
}

/// Blocking HTTPS Range provider suitable for execution-agent worker threads.
#[derive(Clone)]
pub struct HttpRangeTransfer {
    client: Client,
}

impl HttpRangeTransfer {
    pub fn new() -> Result<Self, TransferError> {
        let client = Client::builder()
            .build()
            .map_err(|error| TransferError::Http(error.to_string()))?;
        Ok(Self { client })
    }

    /// Build a client trusting an additional PEM CA for private Artifact services.
    pub fn with_pem_ca(certificate_pem: &[u8]) -> Result<Self, TransferError> {
        let certificate = reqwest::Certificate::from_pem(certificate_pem)
            .map_err(|error| TransferError::Http(error.to_string()))?;
        let client = Client::builder()
            .add_root_certificate(certificate)
            .build()
            .map_err(|error| TransferError::Http(error.to_string()))?;
        Ok(Self { client })
    }

    /// Download missing parts, verify the whole Artifact, then atomically publish.
    pub fn transfer(&self, session: &TransferSession) -> Result<TransferResult, TransferError> {
        session.validate()?;
        if session.destination.exists() {
            verify_file(
                &session.destination,
                &session.manifest.artifact.digest,
                session.manifest.artifact.size_bytes,
            )?;
            return Ok(TransferResult {
                downloaded_parts: 0,
                reused_parts: session.manifest.parts.len(),
            });
        }

        let part_root = part_root(session)?;
        fs::create_dir_all(&part_root)?;
        let checkpoint = load_checkpoint(session, &part_root)?;
        let completed = Arc::new(Mutex::new(checkpoint));
        let pending = session
            .manifest
            .parts
            .iter()
            .filter(|part| {
                !completed
                    .lock()
                    .map(|checkpoint| checkpoint.completed_parts.contains(part))
                    .unwrap_or(false)
            })
            .map(|part| {
                let route = session
                    .plan
                    .route(part.index)
                    .expect("validated transfer plan");
                let source = session
                    .plan
                    .source(route)
                    .expect("validated transfer source");
                (
                    part.clone(),
                    source.replica.locator.clone(),
                    source.ticket.signature.clone(),
                )
            })
            .collect::<VecDeque<_>>();
        let reused_parts = session.manifest.parts.len() - pending.len();
        let downloaded_parts = pending.len();
        let pending = Arc::new(Mutex::new(pending));
        let first_error = Arc::new(Mutex::new(None));

        std::thread::scope(|scope| {
            for _ in 0..session.concurrency.min(downloaded_parts.max(1)) {
                let pending = Arc::clone(&pending);
                let completed = Arc::clone(&completed);
                let first_error = Arc::clone(&first_error);
                let client = self.client.clone();
                let checkpoint_path = session.checkpoint_path.clone();
                let part_root = part_root.clone();
                scope.spawn(move || loop {
                    if first_error.lock().is_ok_and(|error| error.is_some()) {
                        return;
                    }
                    let part = pending
                        .lock()
                        .ok()
                        .and_then(|mut values| values.pop_front());
                    let Some((part, locator, ticket)) = part else {
                        return;
                    };
                    let result = download_part(&client, &locator, &ticket, &part, &part_root)
                        .and_then(|_| {
                            let mut checkpoint = completed.lock().map_err(|_| {
                                TransferError::Checkpoint("checkpoint lock is poisoned".to_string())
                            })?;
                            checkpoint.completed_parts.insert(part);
                            persist_checkpoint(&checkpoint_path, &checkpoint)
                        });
                    if let Err(error) = result {
                        if let Ok(mut first) = first_error.lock() {
                            *first = Some(error);
                        }
                        return;
                    }
                });
            }
        });

        if let Some(error) = first_error.lock().ok().and_then(|mut value| value.take()) {
            return Err(error);
        }
        assemble_and_publish(session, &part_root)?;
        fs::remove_file(&session.checkpoint_path).ok();
        fs::remove_dir_all(&part_root).ok();
        Ok(TransferResult {
            downloaded_parts,
            reused_parts,
        })
    }
}

fn download_part(
    client: &Client,
    locator: &str,
    ticket: &str,
    part: &TransferPart,
    part_root: &Path,
) -> Result<(), TransferError> {
    let final_path = part_path(part_root, part.index);
    if final_path.exists() {
        verify_file(&final_path, &part.digest, part.size_bytes())?;
        return Ok(());
    }
    let response = client
        .get(locator)
        .header(AUTHORIZATION, format!("Bearer {ticket}"))
        .header(
            RANGE,
            format!("bytes={}-{}", part.start, part.end_exclusive - 1),
        )
        .send()
        .map_err(|error| TransferError::Http(error.to_string()))?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(TransferError::Http(format!(
            "replica returned {} instead of 206",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .map_err(|error| TransferError::Http(error.to_string()))?;
    if bytes.len() as u64 != part.size_bytes() || sha256_bytes(&bytes) != part.digest {
        return Err(TransferError::PartDigest(format!(
            "part {} did not match its manifest",
            part.index
        )));
    }
    let temporary = final_path.with_extension("part.tmp");
    let mut handle = File::create(&temporary)?;
    handle.write_all(&bytes)?;
    handle.sync_all()?;
    fs::rename(temporary, final_path)?;
    Ok(())
}

fn assemble_and_publish(session: &TransferSession, part_root: &Path) -> Result<(), TransferError> {
    let parent = session.destination.parent().ok_or_else(|| {
        TransferError::Contract("Artifact destination needs a parent directory".to_string())
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.publish-{}",
        session
            .destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("artifact"),
        session.session_id
    ));
    let result = (|| {
        let mut output = File::create(&temporary)?;
        for part in &session.manifest.parts {
            let mut input = File::open(part_path(part_root, part.index))?;
            std::io::copy(&mut input, &mut output)?;
        }
        output.sync_all()?;
        verify_file(
            &temporary,
            &session.manifest.artifact.digest,
            session.manifest.artifact.size_bytes,
        )?;
        fs::rename(&temporary, &session.destination)?;
        Ok(())
    })();
    if result.is_err() {
        fs::remove_file(&temporary).ok();
    }
    result
}

fn load_checkpoint(
    session: &TransferSession,
    part_root: &Path,
) -> Result<TransferCheckpoint, TransferError> {
    let mut checkpoint = if session.checkpoint_path.exists() {
        let bytes = fs::read(&session.checkpoint_path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| TransferError::Checkpoint(error.to_string()))?
    } else {
        TransferCheckpoint {
            session_id: session.session_id.clone(),
            artifact: session.manifest.artifact.clone(),
            completed_parts: Default::default(),
        }
    };
    if checkpoint.session_id != session.session_id
        || checkpoint.artifact != session.manifest.artifact
        || !checkpoint
            .completed_parts
            .iter()
            .all(|part| session.manifest.parts.contains(part))
    {
        return Err(TransferError::Checkpoint(
            "checkpoint identity does not match transfer session".to_string(),
        ));
    }
    checkpoint.completed_parts.retain(|part| {
        let path = part_path(part_root, part.index);
        path.exists() && verify_file(&path, &part.digest, part.size_bytes()).is_ok()
    });
    for part in &session.manifest.parts {
        if checkpoint.completed_parts.contains(part) {
            continue;
        }
        let path = part_path(part_root, part.index);
        if path.exists() && verify_file(&path, &part.digest, part.size_bytes()).is_ok() {
            checkpoint.completed_parts.insert(part.clone());
        }
    }
    persist_checkpoint(&session.checkpoint_path, &checkpoint)?;
    Ok(checkpoint)
}

fn persist_checkpoint(path: &Path, checkpoint: &TransferCheckpoint) -> Result<(), TransferError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec(checkpoint)
        .map_err(|error| TransferError::Checkpoint(error.to_string()))?;
    let mut handle = File::create(&temporary)?;
    handle.write_all(&bytes)?;
    handle.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn verify_file(path: &Path, digest: &str, size_bytes: u64) -> Result<(), TransferError> {
    let mut input = File::open(path)?;
    let metadata = input.metadata()?;
    if metadata.len() != size_bytes {
        return Err(TransferError::ArtifactDigest(format!(
            "{} has size {}, expected {size_bytes}",
            path.display(),
            metadata.len()
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("sha256:{:x}", hasher.finalize());
    if actual != digest {
        return Err(TransferError::ArtifactDigest(format!(
            "{} has digest {actual}, expected {digest}",
            path.display()
        )));
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn part_root(session: &TransferSession) -> Result<PathBuf, TransferError> {
    let parent = session.checkpoint_path.parent().ok_or_else(|| {
        TransferError::Contract("checkpoint path needs a parent directory".to_string())
    })?;
    Ok(parent.join(format!("{}.parts", session.session_id)))
}

fn part_path(root: &Path, index: u32) -> PathBuf {
    root.join(format!("part-{index:08}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        ArtifactKind, ArtifactPeer, ArtifactPeerKind, ArtifactRef, ArtifactReplica,
        TransferManifest, TransferPartSource, TransferPlan, TransferProtocol, TransferSource,
        TransferTicket,
    };

    fn existing_session(root: &Path, bytes: &[u8], digest: String) -> TransferSession {
        let destination = root.join("published-artifact");
        fs::write(&destination, bytes).unwrap();
        let artifact = ArtifactRef {
            uri: format!("artifact://sha256/{}", &digest[7..]),
            digest: digest.clone(),
            size_bytes: bytes.len() as u64,
            kind: ArtifactKind::generic(),
            manifest_digest: None,
        };
        let manifest = TransferManifest {
            artifact: artifact.clone(),
            part_size_bytes: bytes.len() as u64,
            parts: vec![TransferPart {
                index: 0,
                start: 0,
                end_exclusive: bytes.len() as u64,
                digest,
            }],
        };
        let source = TransferSource {
            peer: ArtifactPeer {
                peer_id: "seed-peer-1".to_string(),
                kind: ArtifactPeerKind::CentralSeed,
                authorized: true,
                residency: "fixture".to_string(),
                trust_domain: "fixture".to_string(),
                classifications: BTreeSet::from(["fixture".to_string()]),
                policy_tags: BTreeSet::new(),
                healthy: true,
                latency_ms: 0,
                bandwidth_mbps: 0,
                cost_microunits: 0,
            },
            replica: ArtifactReplica {
                replica_id: "replica-1".to_string(),
                artifact: artifact.clone(),
                peer_id: "seed-peer-1".to_string(),
                protocol: TransferProtocol::HttpsRangeV1,
                locator: "https://unused.example.test/artifact".to_string(),
                region: None,
                priority: 0,
                expires_at_unix_ms: None,
            },
            ticket: TransferTicket {
                ticket_id: "ticket-1".to_string(),
                artifact: artifact.clone(),
                source_peer_id: "seed-peer-1".to_string(),
                destination_peer_id: "destination-peer".to_string(),
                allowed_parts: BTreeSet::from([0]),
                expires_at_unix_ms: u64::MAX,
                max_bytes: artifact.size_bytes,
                signature: "fixture".to_string(),
            },
        };
        TransferSession {
            session_id: "session-1".to_string(),
            manifest,
            plan: TransferPlan {
                plan_id: "plan-1".to_string(),
                artifact,
                destination_peer_id: "destination-peer".to_string(),
                sources: vec![source],
                part_sources: vec![TransferPartSource {
                    part_index: 0,
                    peer_id: "seed-peer-1".to_string(),
                    replica_id: "replica-1".to_string(),
                }],
            },
            destination,
            checkpoint_path: root.join("checkpoint.json"),
            concurrency: 1,
        }
    }

    #[test]
    fn already_published_artifact_is_verified_without_network_access() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"verified artifact";
        let digest = sha256_bytes(bytes);
        let result = HttpRangeTransfer::new()
            .unwrap()
            .transfer(&existing_session(root.path(), bytes, digest))
            .unwrap();
        assert_eq!(result.downloaded_parts, 0);
        assert_eq!(result.reused_parts, 1);
    }

    #[test]
    fn published_artifact_with_wrong_digest_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let session = existing_session(
            root.path(),
            b"corrupted",
            format!("sha256:{}", "0".repeat(64)),
        );
        assert!(matches!(
            HttpRangeTransfer::new().unwrap().transfer(&session),
            Err(TransferError::ArtifactDigest(_))
        ));
    }

    #[test]
    fn verified_part_written_before_checkpoint_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"verified orphan part";
        let digest = sha256_bytes(bytes);
        let session = existing_session(root.path(), bytes, digest);
        fs::remove_file(&session.destination).unwrap();
        let parts = part_root(&session).unwrap();
        fs::create_dir_all(&parts).unwrap();
        fs::write(part_path(&parts, 0), bytes).unwrap();

        let result = HttpRangeTransfer::new()
            .unwrap()
            .transfer(&session)
            .unwrap();

        assert_eq!(result.downloaded_parts, 0);
        assert_eq!(result.reused_parts, 1);
        assert_eq!(fs::read(&session.destination).unwrap(), bytes);
    }
}
