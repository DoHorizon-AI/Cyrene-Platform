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
        validate_transfer_artifact_ref(&self.artifact)?;
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
        validate_transfer_artifact_ref(&self.artifact)?;
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

/// Deterministic, conservative transfer estimate derived from an authorized
/// [`TransferPlan`]. Cost remains provider-defined, while byte and time values
/// are computed from the canonical part routes and source Peer facts.
///
/// 该估算只消费 Artifact Plane 已生成的计划，不重新选择 Peer 或复制数据策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEstimate {
    pub bytes_to_transfer: u64,
    pub estimated_transfer_millis: u64,
    pub cost_microunits: u64,
    pub source_count: u32,
    pub valid_until_unix_ms: u64,
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
            if source
                .replica
                .expires_at_unix_ms
                .is_some_and(|expires_at| now_unix_ms >= expires_at)
            {
                return Err(TransferError::Authorization(
                    "Artifact replica expired before the planned transfer".to_string(),
                ));
            }
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

    /// Estimate transfer cost from the exact per-part routes in this plan.
    ///
    /// The time estimate is intentionally conservative: routed bytes are
    /// aggregated per source, each source pays one latency cost, and source
    /// times are added rather than assuming unproven cross-source parallelism.
    pub fn estimate(
        &self,
        manifest: &TransferManifest,
        now_unix_ms: u64,
    ) -> Result<TransferEstimate, TransferError> {
        self.validate(manifest, now_unix_ms)?;

        let mut bytes_by_source = BTreeMap::<(&str, &str), u64>::new();
        for route in &self.part_sources {
            let part = manifest
                .parts
                .iter()
                .find(|part| part.index == route.part_index)
                .expect("validated TransferPlan references an existing part");
            let bytes = bytes_by_source
                .entry((route.peer_id.as_str(), route.replica_id.as_str()))
                .or_default();
            *bytes = bytes.checked_add(part.size_bytes()).ok_or_else(|| {
                TransferError::Contract("Transfer estimate byte count overflow".to_string())
            })?;
        }

        let mut estimate = TransferEstimate {
            bytes_to_transfer: 0,
            estimated_transfer_millis: 0,
            cost_microunits: 0,
            source_count: u32::try_from(bytes_by_source.len()).map_err(|_| {
                TransferError::Contract("Transfer estimate source count overflow".to_string())
            })?,
            valid_until_unix_ms: u64::MAX,
        };
        for ((peer_id, replica_id), bytes) in bytes_by_source {
            let source = self
                .sources
                .iter()
                .find(|source| {
                    source.peer.peer_id == peer_id && source.replica.replica_id == replica_id
                })
                .expect("TransferPlan validation covers every source route");
            if source.peer.bandwidth_mbps == 0 {
                return Err(TransferError::Contract(
                    "Transfer estimate requires positive source bandwidth".to_string(),
                ));
            }

            let transfer_millis = u64::try_from(
                (u128::from(bytes) * 8 * 1_000)
                    .div_ceil(u128::from(source.peer.bandwidth_mbps) * 1_000_000),
            )
            .map_err(|_| TransferError::Contract("Transfer estimate time overflow".to_string()))?;
            estimate.bytes_to_transfer =
                estimate
                    .bytes_to_transfer
                    .checked_add(bytes)
                    .ok_or_else(|| {
                        TransferError::Contract("Transfer estimate byte count overflow".to_string())
                    })?;
            estimate.estimated_transfer_millis = estimate
                .estimated_transfer_millis
                .checked_add(source.peer.latency_ms)
                .and_then(|value| value.checked_add(transfer_millis))
                .ok_or_else(|| {
                    TransferError::Contract("Transfer estimate time overflow".to_string())
                })?;
            estimate.cost_microunits = estimate
                .cost_microunits
                .checked_add(source.peer.cost_microunits)
                .ok_or_else(|| {
                    TransferError::Contract("Transfer estimate cost overflow".to_string())
                })?;
            estimate.valid_until_unix_ms = estimate
                .valid_until_unix_ms
                .min(source.ticket.expires_at_unix_ms)
                .min(source.replica.expires_at_unix_ms.unwrap_or(u64::MAX));
        }
        if estimate.bytes_to_transfer != manifest.artifact.size_bytes {
            return Err(TransferError::Contract(
                "Transfer estimate must cover the complete Artifact".to_string(),
            ));
        }
        Ok(estimate)
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
        validate_transfer_artifact_ref(&self.artifact)?;
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

/// Apply range-transfer constraints after canonical Artifact validation.
fn validate_transfer_artifact_ref(artifact: &ArtifactRef) -> Result<(), TransferError> {
    artifact.validate().map_err(TransferError::Contract)?;
    if artifact.size_bytes == 0 {
        return Err(TransferError::Contract(
            "Artifact size must be positive for range transfer".to_string(),
        ));
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
            kind: cy_manifest::ArtifactKind::generic(),
            manifest_digest: None,
        }
    }

    #[test]
    fn artifact_identity_cannot_be_a_source_path() {
        let mut artifact = identity(4);
        artifact.uri = "/mnt/private/model.bin".to_string();
        assert!(matches!(
            validate_transfer_artifact_ref(&artifact),
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

    #[test]
    fn transfer_estimate_uses_authorized_part_routes() {
        let artifact = identity(4);
        let manifest = TransferManifest {
            artifact: artifact.clone(),
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
                    start: 2,
                    end_exclusive: 4,
                    digest: format!("sha256:{}", "2".repeat(64)),
                },
            ],
        };
        let mut plan = TransferPlan {
            plan_id: "estimate-plan".to_string(),
            artifact,
            destination_peer_id: "target-cache".to_string(),
            sources: vec![
                estimate_source("source-a", 10, 1, 1, 0),
                estimate_source("source-b", 20, 1, 2, 1),
            ],
            part_sources: vec![
                TransferPartSource {
                    part_index: 0,
                    peer_id: "source-a".to_string(),
                    replica_id: "replica-source-a".to_string(),
                },
                TransferPartSource {
                    part_index: 1,
                    peer_id: "source-b".to_string(),
                    replica_id: "replica-source-b".to_string(),
                },
            ],
        };
        plan.sources[0].ticket.expires_at_unix_ms = 900;
        plan.sources[1].replica.expires_at_unix_ms = Some(800);

        let estimate = plan.estimate(&manifest, 1).unwrap();

        assert_eq!(estimate.bytes_to_transfer, 4);
        assert_eq!(estimate.estimated_transfer_millis, 32);
        assert_eq!(estimate.cost_microunits, 3);
        assert_eq!(estimate.source_count, 2);
        assert_eq!(estimate.valid_until_unix_ms, 800);
    }

    #[test]
    fn transfer_estimate_rejects_unknown_bandwidth() {
        let artifact = identity(4);
        let manifest = TransferManifest {
            artifact: artifact.clone(),
            part_size_bytes: 4,
            parts: vec![TransferPart {
                index: 0,
                start: 0,
                end_exclusive: 4,
                digest: format!("sha256:{}", "1".repeat(64)),
            }],
        };
        let plan = TransferPlan {
            plan_id: "unknown-bandwidth".to_string(),
            artifact,
            destination_peer_id: "target-cache".to_string(),
            sources: vec![estimate_source("source-a", 10, 0, 1, 0)],
            part_sources: vec![TransferPartSource {
                part_index: 0,
                peer_id: "source-a".to_string(),
                replica_id: "replica-source-a".to_string(),
            }],
        };

        assert!(matches!(
            plan.estimate(&manifest, 1),
            Err(TransferError::Contract(message))
                if message == "Transfer estimate requires positive source bandwidth"
        ));
    }

    #[test]
    fn transfer_plan_rejects_an_expired_replica() {
        let artifact = identity(4);
        let manifest = TransferManifest {
            artifact: artifact.clone(),
            part_size_bytes: 4,
            parts: vec![TransferPart {
                index: 0,
                start: 0,
                end_exclusive: 4,
                digest: format!("sha256:{}", "1".repeat(64)),
            }],
        };
        let mut source = estimate_source("source-a", 10, 1, 1, 0);
        source.replica.expires_at_unix_ms = Some(2);
        let plan = TransferPlan {
            plan_id: "expired-replica".to_string(),
            artifact,
            destination_peer_id: "target-cache".to_string(),
            sources: vec![source],
            part_sources: vec![TransferPartSource {
                part_index: 0,
                peer_id: "source-a".to_string(),
                replica_id: "replica-source-a".to_string(),
            }],
        };

        assert!(matches!(
            plan.estimate(&manifest, 2),
            Err(TransferError::Authorization(message))
                if message == "Artifact replica expired before the planned transfer"
        ));
    }

    fn estimate_source(
        peer_id: &str,
        latency_ms: u64,
        bandwidth_mbps: u64,
        cost_microunits: u64,
        part_index: u32,
    ) -> TransferSource {
        let artifact = identity(4);
        TransferSource {
            peer: ArtifactPeer {
                peer_id: peer_id.to_string(),
                kind: ArtifactPeerKind::CentralSeed,
                authorized: true,
                residency: "us-east".to_string(),
                trust_domain: "workspace-1".to_string(),
                classifications: BTreeSet::from(["internal".to_string()]),
                policy_tags: BTreeSet::from(["training".to_string()]),
                healthy: true,
                latency_ms,
                bandwidth_mbps,
                cost_microunits,
            },
            replica: ArtifactReplica {
                replica_id: format!("replica-{peer_id}"),
                artifact: artifact.clone(),
                peer_id: peer_id.to_string(),
                protocol: TransferProtocol::HttpsRangeV1,
                locator: format!("https://{peer_id}.example.test/value"),
                region: Some("us-east".to_string()),
                priority: 0,
                expires_at_unix_ms: None,
            },
            ticket: TransferTicket {
                ticket_id: format!("ticket-{peer_id}"),
                artifact,
                source_peer_id: peer_id.to_string(),
                destination_peer_id: "target-cache".to_string(),
                allowed_parts: BTreeSet::from([part_index]),
                expires_at_unix_ms: u64::MAX,
                max_bytes: 4,
                signature: "fixture-signature".to_string(),
            },
        }
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
