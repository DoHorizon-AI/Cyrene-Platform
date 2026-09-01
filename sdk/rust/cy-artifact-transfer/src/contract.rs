//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 contract.rs                                                     │
//! │  Module: cy_artifact_transfer::contract                             │
//! │  Role: Replica, session, part, and checkpoint transfer records.     │
//! │                                                                     │
//! │  模块职责：定义 Artifact 副本、session、part 与 checkpoint 记录。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::path::PathBuf;

use cy_manifest::ArtifactRef;
use serde::{Deserialize, Serialize};

use crate::TransferError;

/// Transfer provider selected for one Replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferProtocol {
    HttpsRangeV1,
}

/// One provider-private location for an existing Artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReplica {
    pub replica_id: String,
    pub artifact: ArtifactRef,
    pub protocol: TransferProtocol,
    pub locator: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub region: Option<String>,
    pub priority: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at_unix_ms: Option<u64>,
}

impl ArtifactReplica {
    pub fn validate(&self) -> Result<(), TransferError> {
        validate_artifact_ref(&self.artifact)?;
        if self.replica_id.is_empty()
            || self.protocol != TransferProtocol::HttpsRangeV1
            || !self.locator.starts_with("https://")
        {
            return Err(TransferError::Contract(
                "Artifact replica requires an identity and HTTPS locator".to_string(),
            ));
        }
        Ok(())
    }
}

/// One independently verified byte range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransferPart {
    pub index: u32,
    pub start: u64,
    pub end_exclusive: u64,
    pub digest: String,
}

impl TransferPart {
    pub fn size_bytes(&self) -> u64 {
        self.end_exclusive.saturating_sub(self.start)
    }

    pub fn validate(&self) -> Result<(), TransferError> {
        if self.end_exclusive <= self.start {
            return Err(TransferError::Contract(
                "Transfer part range must be non-empty".to_string(),
            ));
        }
        validate_digest(&self.digest)
    }
}

/// Immutable expected parts for one Artifact transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferManifest {
    pub artifact: ArtifactRef,
    pub part_size_bytes: u64,
    pub parts: Vec<TransferPart>,
}

impl TransferManifest {
    pub fn validate(&self) -> Result<(), TransferError> {
        validate_artifact_ref(&self.artifact)?;
        if self.part_size_bytes == 0 || self.parts.is_empty() {
            return Err(TransferError::Contract(
                "Transfer manifest requires a positive part size and parts".to_string(),
            ));
        }
        let mut next_start = 0;
        for (expected_index, part) in self.parts.iter().enumerate() {
            part.validate()?;
            if part.index != expected_index as u32 || part.start != next_start {
                return Err(TransferError::Contract(
                    "Transfer parts must be contiguous and ordered".to_string(),
                ));
            }
            next_start = part.end_exclusive;
        }
        if next_start != self.artifact.size_bytes {
            return Err(TransferError::Contract(
                "Transfer parts do not cover the Artifact size".to_string(),
            ));
        }
        Ok(())
    }
}

/// Durable completed-part record used across process/container restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCheckpoint {
    pub session_id: String,
    pub artifact: ArtifactRef,
    pub completed_parts: BTreeSet<TransferPart>,
}

/// One resumable transfer with provider-private local paths.
#[derive(Debug, Clone)]
pub struct TransferSession {
    pub session_id: String,
    pub manifest: TransferManifest,
    pub replica: ArtifactReplica,
    pub destination: PathBuf,
    pub checkpoint_path: PathBuf,
    pub concurrency: usize,
}

impl TransferSession {
    pub fn validate(&self) -> Result<(), TransferError> {
        self.manifest.validate()?;
        self.replica.validate()?;
        if self.session_id.is_empty()
            || self.concurrency == 0
            || self.replica.artifact != self.manifest.artifact
        {
            return Err(TransferError::Contract(
                "Transfer session identity, concurrency, or replica is invalid".to_string(),
            ));
        }
        if self.destination == self.checkpoint_path {
            return Err(TransferError::Contract(
                "Artifact destination and checkpoint path must differ".to_string(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_digest(value: &str) -> Result<(), TransferError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(TransferError::Contract(
            "digest must be lowercase sha256:<hex>".to_string(),
        ))
    }
}

fn validate_artifact_ref(artifact: &ArtifactRef) -> Result<(), TransferError> {
    validate_digest(&artifact.digest)?;
    if artifact.uri != format!("artifact://sha256/{}", &artifact.digest[7..]) {
        return Err(TransferError::Contract(
            "Artifact URI does not match its canonical digest".to_string(),
        ));
    }
    if artifact.size_bytes == 0 {
        return Err(TransferError::Contract(
            "Artifact size must be positive for range transfer".to_string(),
        ));
    }
    if let Some(manifest_digest) = &artifact.manifest_digest {
        validate_digest(manifest_digest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(size_bytes: u64) -> ArtifactRef {
        ArtifactRef {
            uri: format!("artifact://sha256/{}", "0".repeat(64)),
            digest: format!("sha256:{}", "0".repeat(64)),
            size_bytes,
            kind: cy_manifest::ArtifactKind::Generic,
            manifest_digest: None,
        }
    }

    #[test]
    fn artifact_identity_cannot_be_a_source_path() {
        let mut artifact = identity(4);
        artifact.uri = "/mnt/private/model.bin".to_string();
        assert!(matches!(
            validate_artifact_ref(&artifact),
            Err(TransferError::Contract(_))
        ));
    }

    #[test]
    fn manifest_rejects_a_gap_between_parts() {
        let manifest = TransferManifest {
            artifact: identity(4),
            part_size_bytes: 2,
            parts: vec![
                TransferPart {
                    index: 0,
                    start: 0,
                    end_exclusive: 2,
                    digest: format!("sha256:{}", "1".repeat(64)),
                },
                TransferPart {
                    index: 1,
                    start: 3,
                    end_exclusive: 4,
                    digest: format!("sha256:{}", "2".repeat(64)),
                },
            ],
        };
        assert!(matches!(
            manifest.validate(),
            Err(TransferError::Contract(_))
        ));
    }

    #[test]
    fn replica_serialization_matches_the_frozen_schema() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/schemas/artifact_transfer.schema.json"
        ))
        .unwrap();
        let compiled = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .compile(&schema)
            .unwrap();
        let replica = ArtifactReplica {
            replica_id: "replica-1".to_string(),
            artifact: identity(4),
            protocol: TransferProtocol::HttpsRangeV1,
            locator: "https://artifact.example.test/value".to_string(),
            region: None,
            priority: 0,
            expires_at_unix_ms: None,
        };
        let serialized = serde_json::to_value(replica).unwrap();
        assert!(compiled.validate(&serialized).is_ok());
        assert!(serialized.get("region").is_none());
    }
}
