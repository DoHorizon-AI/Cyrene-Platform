//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 contract.rs                                                     │
//! │  Module: cy_artifact_transfer::contract                             │
//! │  Role: Replica, session, part, and checkpoint transfer records.     │
//! │                                                                     │
//! │  模块职责：定义 Artifact 副本、session、part 与 checkpoint 记录。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
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

/// Artifact transfer endpoint category. A Peer is never a Node identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPeerKind {
    CentralSeed,
    RegionalCache,
    ObjectStoreGateway,
    DesktopCache,
    NodeCache,
    Generic,
}

/// Policy and quality facts for one Artifact transfer Peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPeer {
    pub peer_id: String,
    pub kind: ArtifactPeerKind,
    pub authorized: bool,
    pub residency: String,
    pub trust_domain: String,
    pub classifications: BTreeSet<String>,
    pub policy_tags: BTreeSet<String>,
    pub healthy: bool,
    pub latency_ms: u64,
    pub bandwidth_mbps: u64,
    pub cost_microunits: u64,
}

impl ArtifactPeer {
    pub fn validate(&self) -> Result<(), TransferError> {
        if self.peer_id.is_empty() || self.residency.is_empty() || self.trust_domain.is_empty() {
            return Err(TransferError::Contract(
                "Artifact Peer identity, residency, and trust domain are required".to_string(),
            ));
        }
        Ok(())
    }
}

/// One provider-private location for an existing Artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReplica {
    pub replica_id: String,
    pub artifact: ArtifactRef,
    pub peer_id: String,
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
            || self.peer_id.is_empty()
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

/// Policy-bearing directory result before a destination-scoped ticket exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSourceCandidate {
    pub peer: ArtifactPeer,
    pub replica: ArtifactReplica,
}

/// Short-lived authorization for Peer-to-Peer part transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferTicket {
    pub ticket_id: String,
    pub artifact: ArtifactRef,
    pub source_peer_id: String,
    pub destination_peer_id: String,
    pub allowed_parts: BTreeSet<u32>,
    pub expires_at_unix_ms: u64,
    pub max_bytes: u64,
    pub signature: String,
}

impl TransferTicket {
    pub fn validate(&self) -> Result<(), TransferError> {
        validate_artifact_ref(&self.artifact)?;
        if self.ticket_id.is_empty()
            || self.source_peer_id.is_empty()
            || self.destination_peer_id.is_empty()
            || self.allowed_parts.is_empty()
            || self.expires_at_unix_ms == 0
            || self.max_bytes == 0
            || self.signature.is_empty()
        {
            return Err(TransferError::Contract(
                "TransferTicket requires scope, expiry, byte limit, and signature".to_string(),
            ));
        }
        Ok(())
    }

    pub fn authorizes(
        &self,
        artifact: &ArtifactRef,
        source_peer_id: &str,
        destination_peer_id: &str,
        part: &TransferPart,
        now_unix_ms: u64,
    ) -> bool {
        self.artifact == *artifact
            && self.source_peer_id == source_peer_id
            && self.destination_peer_id == destination_peer_id
            && self.allowed_parts.contains(&part.index)
            && part.size_bytes() <= self.max_bytes
            && now_unix_ms < self.expires_at_unix_ms
    }
}

/// One authorized source and concrete Replica for a transfer plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSource {
    pub peer: ArtifactPeer,
    pub replica: ArtifactReplica,
    pub ticket: TransferTicket,
}

/// Per-part source assignment. Different parts may select different Peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPartSource {
    pub part_index: u32,
    pub peer_id: String,
    pub replica_id: String,
}

/// Multi-source-ready routing around one canonical Artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPlan {
    pub plan_id: String,
    pub artifact: ArtifactRef,
    pub destination_peer_id: String,
    pub sources: Vec<TransferSource>,
    pub part_sources: Vec<TransferPartSource>,
}

impl TransferPlan {
    pub fn validate(
        &self,
        manifest: &TransferManifest,
        now_unix_ms: u64,
    ) -> Result<(), TransferError> {
        manifest.validate()?;
        if self.plan_id.is_empty()
            || self.destination_peer_id.is_empty()
            || self.artifact != manifest.artifact
            || self.sources.is_empty()
            || self.part_sources.len() != manifest.parts.len()
        {
            return Err(TransferError::Contract(
                "TransferPlan identity, destination, sources, and part routes are required"
                    .to_string(),
            ));
        }
        let mut routed = BTreeSet::new();
        let mut ticket_bytes = BTreeMap::<String, u64>::new();
        for route in &self.part_sources {
            if !routed.insert(route.part_index) {
                return Err(TransferError::Contract(
                    "TransferPlan routes each part exactly once".to_string(),
                ));
            }
            let part = manifest
                .parts
                .iter()
                .find(|part| part.index == route.part_index)
                .ok_or_else(|| {
                    TransferError::Contract("TransferPlan references an unknown part".to_string())
                })?;
            let source = self.source(route).ok_or_else(|| {
                TransferError::Contract(
                    "TransferPlan references an unknown Peer replica".to_string(),
                )
            })?;
            source.peer.validate()?;
            source.replica.validate()?;
            source.ticket.validate()?;
            if source.replica.artifact != self.artifact
                || source.replica.peer_id != source.peer.peer_id
                || !source.ticket.authorizes(
                    &self.artifact,
                    &route.peer_id,
                    &self.destination_peer_id,
                    part,
                    now_unix_ms,
                )
            {
                return Err(TransferError::Authorization(
                    "TransferTicket does not authorize the planned part route".to_string(),
                ));
            }
            let transferred = ticket_bytes
                .entry(source.ticket.ticket_id.clone())
                .or_default();
            *transferred = transferred.saturating_add(part.size_bytes());
            if *transferred > source.ticket.max_bytes {
                return Err(TransferError::Authorization(
                    "TransferTicket byte budget is exceeded".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn source(&self, route: &TransferPartSource) -> Option<&TransferSource> {
        self.sources.iter().find(|source| {
            source.peer.peer_id == route.peer_id && source.replica.replica_id == route.replica_id
        })
    }

    pub fn route(&self, part_index: u32) -> Option<&TransferPartSource> {
        self.part_sources
            .iter()
            .find(|route| route.part_index == part_index)
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
    pub plan: TransferPlan,
    pub destination: PathBuf,
    pub checkpoint_path: PathBuf,
    pub concurrency: usize,
}

impl TransferSession {
    pub fn validate(&self) -> Result<(), TransferError> {
        self.manifest.validate()?;
        self.plan.validate(&self.manifest, crate::now_unix_ms())?;
        if self.session_id.is_empty() || self.concurrency == 0 {
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
        let compiled = compiled_schema();
        let replica = ArtifactReplica {
            replica_id: "replica-1".to_string(),
            artifact: identity(4),
            peer_id: "seed-peer-1".to_string(),
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

    #[test]
    fn multi_source_plan_serialization_matches_the_frozen_schema() {
        let artifact = identity(4);
        let source = TransferSource {
            peer: ArtifactPeer {
                peer_id: "seed-peer-1".to_string(),
                kind: ArtifactPeerKind::CentralSeed,
                authorized: true,
                residency: "us-east".to_string(),
                trust_domain: "workspace-1".to_string(),
                classifications: BTreeSet::from(["internal".to_string()]),
                policy_tags: BTreeSet::from(["training".to_string()]),
                healthy: true,
                latency_ms: 10,
                bandwidth_mbps: 1_000,
                cost_microunits: 1,
            },
            replica: ArtifactReplica {
                replica_id: "replica-1".to_string(),
                artifact: artifact.clone(),
                peer_id: "seed-peer-1".to_string(),
                protocol: TransferProtocol::HttpsRangeV1,
                locator: "https://artifact.example.test/value".to_string(),
                region: Some("us-east".to_string()),
                priority: 0,
                expires_at_unix_ms: None,
            },
            ticket: TransferTicket {
                ticket_id: "ticket-1".to_string(),
                artifact: artifact.clone(),
                source_peer_id: "seed-peer-1".to_string(),
                destination_peer_id: "node-cache-1".to_string(),
                allowed_parts: BTreeSet::from([0, 1]),
                expires_at_unix_ms: u64::MAX,
                max_bytes: 4,
                signature: "fixture-signature".to_string(),
            },
        };
        let plan = TransferPlan {
            plan_id: "plan-1".to_string(),
            artifact,
            destination_peer_id: "node-cache-1".to_string(),
            sources: vec![source],
            part_sources: vec![
                TransferPartSource {
                    part_index: 0,
                    peer_id: "seed-peer-1".to_string(),
                    replica_id: "replica-1".to_string(),
                },
                TransferPartSource {
                    part_index: 1,
                    peer_id: "seed-peer-1".to_string(),
                    replica_id: "replica-1".to_string(),
                },
            ],
        };
        assert!(compiled_schema()
            .validate(&serde_json::to_value(plan).unwrap())
            .is_ok());
    }

    fn compiled_schema() -> jsonschema::JSONSchema {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/schemas/artifact_transfer.schema.json"
        ))
        .unwrap();
        jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .compile(&schema)
            .unwrap()
    }
}
