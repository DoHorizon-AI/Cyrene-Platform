//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 planner.rs                                                      │
//! │  Module: cy_artifact_transfer::planner                              │
//! │  Role: Policy-first Peer resolution and transfer planning.          │
//! │                                                                     │
//! │  模块职责：先执行授权与数据策略，再按质量选择 Peer 并生成传输计划。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;

use cy_manifest::ArtifactRef;

use crate::{
    ArtifactSourceCandidate, TransferError, TransferManifest, TransferPartSource, TransferPlan,
    TransferSource, TransferTicketIssuer, TransferTicketRequest,
};

/// Source-directory seam owned by the Artifact Plane implementation.
pub trait ArtifactSourceResolver: Send + Sync {
    fn resolve_sources(
        &self,
        artifact: &ArtifactRef,
    ) -> Result<Vec<ArtifactSourceCandidate>, TransferError>;
}

/// Reference in-memory Peer/Replica directory; Artifact identity remains external.
#[derive(Debug, Clone, Default)]
pub struct InMemoryArtifactPeerDirectory {
    sources: Vec<ArtifactSourceCandidate>,
}

impl InMemoryArtifactPeerDirectory {
    pub fn new(sources: Vec<ArtifactSourceCandidate>) -> Self {
        Self { sources }
    }
}

impl ArtifactSourceResolver for InMemoryArtifactPeerDirectory {
    fn resolve_sources(
        &self,
        artifact: &ArtifactRef,
    ) -> Result<Vec<ArtifactSourceCandidate>, TransferError> {
        Ok(self
            .sources
            .iter()
            .filter(|source| source.replica.artifact == *artifact)
            .cloned()
            .collect())
    }
}

/// Workspace policy evaluated before any latency, bandwidth, or cost score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSelectionPolicy {
    pub allowed_peer_ids: BTreeSet<String>,
    pub allowed_residencies: BTreeSet<String>,
    pub allowed_trust_domains: BTreeSet<String>,
    pub classification: String,
    pub required_policy_tags: BTreeSet<String>,
}

impl PeerSelectionPolicy {
    fn admit(&self, source: &ArtifactSourceCandidate) -> Result<(), TransferError> {
        if !source.peer.authorized || !self.allowed_peer_ids.contains(&source.peer.peer_id) {
            return Err(TransferError::Policy("PEER_NOT_AUTHORIZED".to_string()));
        }
        if !self.allowed_residencies.contains(&source.peer.residency) {
            return Err(TransferError::Policy("RESIDENCY_DENIED".to_string()));
        }
        if !self
            .allowed_trust_domains
            .contains(&source.peer.trust_domain)
        {
            return Err(TransferError::Policy("TRUST_DOMAIN_DENIED".to_string()));
        }
        if !source.peer.classifications.contains(&self.classification) {
            return Err(TransferError::Policy("CLASSIFICATION_DENIED".to_string()));
        }
        if !self
            .required_policy_tags
            .is_subset(&source.peer.policy_tags)
        {
            return Err(TransferError::Policy("POLICY_TAG_DENIED".to_string()));
        }
        Ok(())
    }
}

/// Planner seam for future multi-Peer and P2P acceleration strategies.
#[derive(Debug, Clone, Copy)]
pub struct TransferPlanningRequest<'a> {
    pub plan_id: &'a str,
    pub manifest: &'a TransferManifest,
    pub destination_peer_id: &'a str,
    pub policy: &'a PeerSelectionPolicy,
    pub now_unix_ms: u64,
}

/// Planner seam for future multi-Peer and P2P acceleration strategies.
pub trait TransferPlanner: Send + Sync {
    fn plan(
        &self,
        request: TransferPlanningRequest<'_>,
        candidates: Vec<ArtifactSourceCandidate>,
        ticket_issuer: &dyn TransferTicketIssuer,
    ) -> Result<TransferPlan, TransferError>;
}

/// Single Artifact-plane coordinator combining directory resolution and planning.
#[derive(Debug, Clone)]
pub struct ArtifactTransferCoordinator<R, P, I> {
    resolver: R,
    planner: P,
    ticket_issuer: I,
}

impl<R, P, I> ArtifactTransferCoordinator<R, P, I>
where
    R: ArtifactSourceResolver,
    P: TransferPlanner,
    I: TransferTicketIssuer,
{
    pub fn new(resolver: R, planner: P, ticket_issuer: I) -> Self {
        Self {
            resolver,
            planner,
            ticket_issuer,
        }
    }

    pub fn resolve_and_plan(
        &self,
        plan_id: &str,
        manifest: &TransferManifest,
        destination_peer_id: &str,
        policy: &PeerSelectionPolicy,
        now_unix_ms: u64,
    ) -> Result<TransferPlan, TransferError> {
        let candidates = self.resolver.resolve_sources(&manifest.artifact)?;
        self.planner.plan(
            TransferPlanningRequest {
                plan_id,
                manifest,
                destination_peer_id,
                policy,
                now_unix_ms,
            },
            candidates,
            &self.ticket_issuer,
        )
    }
}

/// MVP planner: all parts use the best policy-admitted seed/source Peer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeedFirstTransferPlanner;

impl TransferPlanner for SeedFirstTransferPlanner {
    fn plan(
        &self,
        request: TransferPlanningRequest<'_>,
        candidates: Vec<ArtifactSourceCandidate>,
        ticket_issuer: &dyn TransferTicketIssuer,
    ) -> Result<TransferPlan, TransferError> {
        let TransferPlanningRequest {
            plan_id,
            manifest,
            destination_peer_id,
            policy,
            now_unix_ms,
        } = request;
        manifest.validate()?;
        let mut eligible = candidates
            .into_iter()
            .filter(|source| policy.admit(source).is_ok())
            .filter(|source| source.peer.healthy)
            .collect::<Vec<_>>();
        eligible.sort_by(|left, right| {
            left.peer
                .latency_ms
                .cmp(&right.peer.latency_ms)
                .then_with(|| right.peer.bandwidth_mbps.cmp(&left.peer.bandwidth_mbps))
                .then_with(|| left.peer.cost_microunits.cmp(&right.peer.cost_microunits))
                .then_with(|| left.peer.peer_id.cmp(&right.peer.peer_id))
        });
        let source = eligible.into_iter().next().ok_or_else(|| {
            TransferError::Policy("NO_POLICY_COMPLIANT_ARTIFACT_PEER".to_string())
        })?;
        let ticket = ticket_issuer.issue(TransferTicketRequest {
            ticket_id: format!("{plan_id}-{}", source.peer.peer_id),
            artifact: manifest.artifact.clone(),
            source_peer_id: source.peer.peer_id.clone(),
            destination_peer_id: destination_peer_id.to_string(),
            allowed_parts: manifest.parts.iter().map(|part| part.index).collect(),
            expires_at_unix_ms: now_unix_ms.saturating_add(5 * 60 * 1000),
            max_bytes: manifest.artifact.size_bytes,
        })?;
        let part_sources = manifest
            .parts
            .iter()
            .map(|part| TransferPartSource {
                part_index: part.index,
                peer_id: source.peer.peer_id.clone(),
                replica_id: source.replica.replica_id.clone(),
            })
            .collect();
        Ok(TransferPlan {
            plan_id: plan_id.to_string(),
            artifact: manifest.artifact.clone(),
            destination_peer_id: destination_peer_id.to_string(),
            sources: vec![TransferSource {
                peer: source.peer,
                replica: source.replica,
                ticket,
            }],
            part_sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cy_manifest::{ArtifactKind, ArtifactRef};

    use crate::{
        ArtifactPeer, ArtifactPeerKind, ArtifactReplica, DevelopmentTransferTicketAuthority,
        TransferPart, TransferProtocol, TransferTicket,
    };

    use super::*;

    fn artifact() -> ArtifactRef {
        ArtifactRef {
            uri: format!("artifact://sha256/{}", "0".repeat(64)),
            digest: format!("sha256:{}", "0".repeat(64)),
            size_bytes: 4,
            kind: ArtifactKind::generic(),
            manifest_digest: None,
        }
    }

    fn source(id: &str, authorized: bool, latency_ms: u64) -> ArtifactSourceCandidate {
        let artifact = artifact();
        ArtifactSourceCandidate {
            peer: ArtifactPeer {
                peer_id: id.to_string(),
                kind: ArtifactPeerKind::CentralSeed,
                authorized,
                residency: "us".to_string(),
                trust_domain: "workspace".to_string(),
                classifications: BTreeSet::from(["internal".to_string()]),
                policy_tags: BTreeSet::from(["restricted".to_string()]),
                healthy: true,
                latency_ms,
                bandwidth_mbps: 100,
                cost_microunits: 1,
            },
            replica: ArtifactReplica {
                replica_id: format!("replica-{id}"),
                artifact: artifact.clone(),
                peer_id: id.to_string(),
                protocol: TransferProtocol::HttpsRangeV1,
                locator: format!("https://{id}.example/artifact"),
                region: Some("us".to_string()),
                priority: 0,
                expires_at_unix_ms: None,
            },
        }
    }

    fn manifest() -> TransferManifest {
        TransferManifest {
            artifact: artifact(),
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
        }
    }

    fn policy() -> PeerSelectionPolicy {
        PeerSelectionPolicy {
            allowed_peer_ids: BTreeSet::from(["denied-fast".to_string(), "allowed".to_string()]),
            allowed_residencies: BTreeSet::from(["us".to_string()]),
            allowed_trust_domains: BTreeSet::from(["workspace".to_string()]),
            classification: "internal".to_string(),
            required_policy_tags: BTreeSet::from(["restricted".to_string()]),
        }
    }

    #[test]
    fn authorization_precedes_fastest_peer_scoring() {
        let directory = InMemoryArtifactPeerDirectory::new(vec![
            source("denied-fast", false, 1),
            source("allowed", true, 20),
        ]);
        let issuer = DevelopmentTransferTicketAuthority::new([7_u8; 32]).unwrap();
        let plan = ArtifactTransferCoordinator::new(directory, SeedFirstTransferPlanner, issuer)
            .resolve_and_plan("plan-1", &manifest(), "destination", &policy(), 1)
            .unwrap();
        assert!(plan
            .part_sources
            .iter()
            .all(|route| route.peer_id == "allowed"));
    }

    #[test]
    fn contract_accepts_parts_routed_to_different_peers() {
        let manifest = manifest();
        let first = transfer_source(source("allowed", true, 20), BTreeSet::from([0]));
        let second = transfer_source(source("second", true, 30), BTreeSet::from([1]));
        let plan = TransferPlan {
            plan_id: "multi-source".to_string(),
            artifact: artifact(),
            destination_peer_id: "destination".to_string(),
            sources: vec![first, second],
            part_sources: vec![
                TransferPartSource {
                    part_index: 0,
                    peer_id: "allowed".to_string(),
                    replica_id: "replica-allowed".to_string(),
                },
                TransferPartSource {
                    part_index: 1,
                    peer_id: "second".to_string(),
                    replica_id: "replica-second".to_string(),
                },
            ],
        };
        assert!(plan.validate(&manifest, 1).is_ok());
    }

    fn transfer_source(
        candidate: ArtifactSourceCandidate,
        allowed_parts: BTreeSet<u32>,
    ) -> TransferSource {
        let artifact = candidate.replica.artifact.clone();
        let source_peer_id = candidate.peer.peer_id.clone();
        TransferSource {
            peer: candidate.peer,
            replica: candidate.replica,
            ticket: TransferTicket {
                ticket_id: format!("ticket-{source_peer_id}"),
                artifact,
                source_peer_id,
                destination_peer_id: "destination".to_string(),
                allowed_parts,
                expires_at_unix_ms: u64::MAX,
                max_bytes: 4,
                signature: "fixture".to_string(),
            },
        }
    }
}
