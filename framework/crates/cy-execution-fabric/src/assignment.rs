//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 assignment.rs                                                   │
//! │  Module: cy_execution_fabric::assignment                            │
//! │  Role: Bind canonical Kernel Leases to Runtime assignments.         │
//! │                                                                     │
//! │  模块职责：把 Kernel 权威签发的 Lease 绑定到 Runtime assignment。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};

use cy_kernel_contract as semantic;
use cy_manifest::{ArtifactKind, ArtifactRef};
use cy_proto::{core_v1, semantic_v1};

use crate::placement::{
    ArtifactAvailability, ArtifactPlacementQuote, ExecutionPlacementRequest,
    ExecutionTargetCandidate,
};
use crate::{admission::validate_assignment_payload, validate_assignment, FabricContractError};

/// Immutable non-authority fields supplied by a Product-neutral controller.
///
/// The builder never chooses a target, allocates resources, or creates Lease,
/// fence, Runtime, Operation, Attempt, or workload identities. Callers must
/// persist those identities before invoking the canonical Kernel authority.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeAssignmentBuilder {
    assignment_id: String,
    runtime: semantic::Identity,
    operation: semantic::Identity,
    attempt_id: String,
    workload_identity: core_v1::WorkloadIdentity,
    profile: core_v1::RuntimeProfile,
    artifacts: Vec<core_v1::ArtifactTransferSpec>,
}

impl RuntimeAssignmentBuilder {
    pub fn new(
        assignment_id: impl Into<String>,
        runtime: semantic::Identity,
        operation: semantic::Identity,
        attempt_id: impl Into<String>,
        workload_identity: core_v1::WorkloadIdentity,
        profile: core_v1::RuntimeProfile,
        artifacts: Vec<core_v1::ArtifactTransferSpec>,
    ) -> Self {
        Self {
            assignment_id: assignment_id.into(),
            runtime,
            operation,
            attempt_id: attempt_id.into(),
            workload_identity,
            profile,
            artifacts,
        }
    }

    pub fn runtime(&self) -> &semantic::Identity {
        &self.runtime
    }

    pub fn assignment_id(&self) -> &str {
        &self.assignment_id
    }

    pub fn workload_identity(&self) -> &core_v1::WorkloadIdentity {
        &self.workload_identity
    }

    /// Validate the immutable Artifact projection after placement selects one
    /// candidate. Transfer specs are checked against the requested Artifact
    /// identity and the selected Artifact Peer, but replica selection and
    /// ticket issuance remain outside Framework.
    ///
    /// Core v1 exposes only transfer specs, not an explicit local-input
    /// projection. `VerifiedLocal` Artifacts therefore fail closed because
    /// omitting them would prevent the Runtime from verifying their identity.
    /// Zero-byte remote transfers and non-Generic remote Artifacts also fail
    /// closed because the wire cannot represent them without losing semantics.
    pub fn validate_artifact_projection(
        &self,
        placement: &ExecutionPlacementRequest,
        selected: &ExecutionTargetCandidate,
    ) -> Result<(), FabricContractError> {
        let requested = index_requested_artifacts(&placement.artifacts)?;
        let assignments = index_assignment_artifacts(&self.artifacts)?;
        let quotes = index_artifact_quotes(&selected.artifact_quotes)?;

        if requested.is_empty() {
            if let Some((digest, _)) = assignments.iter().next() {
                return Err(FabricContractError::new(
                    "ARTIFACT_ASSIGNMENT_UNEXPECTED",
                    format!("assignment contains an Artifact absent from placement: {digest}"),
                ));
            }
            if let Some((digest, _)) = quotes.iter().next() {
                return Err(FabricContractError::new(
                    "ARTIFACT_QUOTE_SET_MISMATCH",
                    format!("candidate contains an Artifact quote absent from placement: {digest}"),
                ));
            }
            return Ok(());
        }

        let destination = &selected.artifact_destination_peer_id;
        if destination.is_empty() || destination == &selected.node.node_id {
            return Err(FabricContractError::new(
                "ARTIFACT_DESTINATION_INVALID",
                "selected candidate requires an explicit Artifact Peer distinct from Node identity",
            ));
        }

        if quotes.len() != requested.len() {
            return Err(FabricContractError::new(
                "ARTIFACT_QUOTE_SET_MISMATCH",
                "selected candidate Artifact quote set does not exactly match placement",
            ));
        }

        let expected_transfer_digests = requested
            .keys()
            .filter_map(|digest| {
                quotes.get(digest).and_then(|quote| {
                    matches!(
                        quote.availability,
                        ArtifactAvailability::AuthorizedTransfer(_)
                    )
                    .then_some(digest.clone())
                })
            })
            .collect::<BTreeSet<_>>();
        let assignment_digests = assignments.keys().cloned().collect::<BTreeSet<_>>();
        if let Some(digest) = expected_transfer_digests
            .difference(&assignment_digests)
            .next()
        {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_MISSING",
                format!("assignment lacks the transfer projection for Artifact {digest}"),
            ));
        }
        if let Some(digest) = assignment_digests
            .difference(&expected_transfer_digests)
            .next()
        {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_UNEXPECTED",
                format!("assignment transfers an Artifact not authorized by placement: {digest}"),
            ));
        }

        for (digest, artifact) in requested {
            let quote = quotes.get(&digest).ok_or_else(|| {
                FabricContractError::new(
                    "ARTIFACT_QUOTE_MISSING",
                    format!("selected candidate lacks an Artifact quote for {digest}"),
                )
            })?;
            if quote.artifact != *artifact {
                return Err(FabricContractError::new(
                    "ARTIFACT_QUOTE_IDENTITY_MISMATCH",
                    format!("Artifact quote disagrees with placement identity for {digest}"),
                ));
            }
            if quote.destination_peer_id != *destination
                || quote.policy_scope != placement.artifact_policy_scope
            {
                return Err(FabricContractError::new(
                    "ARTIFACT_QUOTE_SCOPE_MISMATCH",
                    format!(
                        "Artifact quote scope disagrees with selected destination for {digest}"
                    ),
                ));
            }

            match quote.availability {
                ArtifactAvailability::VerifiedLocal { .. } => {
                    return Err(FabricContractError::new(
                        "ARTIFACT_LOCAL_INPUT_UNREPRESENTABLE",
                        format!(
                            "Core v1 RuntimeAssignment cannot preserve local Artifact identity for {digest}"
                        ),
                    ));
                }
                ArtifactAvailability::AuthorizedTransfer(_) => {
                    if artifact.size_bytes == 0 {
                        return Err(FabricContractError::new(
                            "ARTIFACT_ZERO_BYTE_TRANSFER_UNSUPPORTED",
                            format!("zero-byte remote Artifact cannot be projected for transfer: {digest}"),
                        ));
                    }
                    if artifact.kind != ArtifactKind::Generic {
                        return Err(FabricContractError::new(
                            "ARTIFACT_TYPE_UNREPRESENTABLE",
                            format!("Core v1 transfer projection cannot preserve Artifact type for {digest}"),
                        ));
                    }
                    let spec = assignments.get(&digest).ok_or_else(|| {
                        FabricContractError::new(
                            "ARTIFACT_ASSIGNMENT_MISSING",
                            format!(
                                "assignment lacks the transfer projection for Artifact {digest}"
                            ),
                        )
                    })?;
                    let projected = artifact_ref_from_transfer_spec(spec)?;
                    if projected != *artifact {
                        return Err(FabricContractError::new(
                            "ARTIFACT_ASSIGNMENT_IDENTITY_MISMATCH",
                            format!("assignment transfer disagrees with placement identity for {digest}"),
                        ));
                    }
                    if spec.destination_peer_id != *destination {
                        return Err(FabricContractError::new(
                            "ARTIFACT_DESTINATION_MISMATCH",
                            format!("assignment transfer destination disagrees with selected Peer for {digest}"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Validate all assignment fields that do not require Lease authority.
    ///
    /// Controllers call this before `acquire_lease`, so malformed payloads do
    /// not consume resources and wait for expiry as their cleanup mechanism.
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), FabricContractError> {
        let assignment = self.assignment(None);
        validate_assignment_payload(&self.runtime, &assignment, now_unix_ms).map(|_| ())
    }

    /// Bind an authority-returned Lease and run the same admission validation
    /// that the Runtime Agent applies before starting its workload.
    pub fn build(
        &self,
        lease: &semantic::Lease,
        now_unix_ms: u64,
    ) -> Result<core_v1::RuntimeAssignment, FabricContractError> {
        self.validate(now_unix_ms)?;
        let assignment = self.assignment(Some(semantic_lease_to_proto(lease)));
        validate_assignment(&self.runtime, &assignment, now_unix_ms)?;
        Ok(assignment)
    }

    fn assignment(&self, lease: Option<semantic_v1::Lease>) -> core_v1::RuntimeAssignment {
        core_v1::RuntimeAssignment {
            assignment_id: self.assignment_id.clone(),
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(identity_to_proto(&self.runtime)),
            }),
            operation: Some(identity_to_proto(&self.operation)),
            attempt_id: self.attempt_id.clone(),
            lease,
            workload_identity: Some(self.workload_identity.clone()),
            profile: Some(self.profile.clone()),
            desired_state: core_v1::DesiredRuntimeState::Running as i32,
            artifacts: self.artifacts.clone(),
        }
    }
}

fn index_requested_artifacts(
    artifacts: &[ArtifactRef],
) -> Result<BTreeMap<String, &ArtifactRef>, FabricContractError> {
    let mut indexed = BTreeMap::new();
    for artifact in artifacts {
        artifact.validate().map_err(|message| {
            FabricContractError::new(
                "ARTIFACT_IDENTITY_INVALID",
                format!("placement Artifact is invalid: {message}"),
            )
        })?;
        if indexed.insert(artifact.digest.clone(), artifact).is_some() {
            return Err(FabricContractError::new(
                "ARTIFACT_PLACEMENT_DUPLICATE",
                format!(
                    "placement contains a duplicate Artifact digest: {}",
                    artifact.digest
                ),
            ));
        }
    }
    Ok(indexed)
}

fn index_assignment_artifacts(
    artifacts: &[core_v1::ArtifactTransferSpec],
) -> Result<BTreeMap<String, &core_v1::ArtifactTransferSpec>, FabricContractError> {
    let mut indexed = BTreeMap::new();
    for spec in artifacts {
        let projected = artifact_ref_from_transfer_spec(spec)?;
        if indexed.insert(projected.digest.clone(), spec).is_some() {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_DUPLICATE",
                format!(
                    "assignment contains a duplicate Artifact digest: {}",
                    projected.digest
                ),
            ));
        }
    }
    Ok(indexed)
}

fn index_artifact_quotes(
    quotes: &[ArtifactPlacementQuote],
) -> Result<BTreeMap<String, &ArtifactPlacementQuote>, FabricContractError> {
    let mut indexed = BTreeMap::new();
    let mut quote_ids = BTreeSet::new();
    for quote in quotes {
        quote.artifact.validate().map_err(|message| {
            FabricContractError::new(
                "ARTIFACT_QUOTE_INVALID",
                format!("candidate Artifact quote is invalid: {message}"),
            )
        })?;
        if quote.quote_id.is_empty() || !quote_ids.insert(quote.quote_id.clone()) {
            return Err(FabricContractError::new(
                "ARTIFACT_QUOTE_DUPLICATE",
                "candidate Artifact quotes require unique non-empty identities",
            ));
        }
        if indexed
            .insert(quote.artifact.digest.clone(), quote)
            .is_some()
        {
            return Err(FabricContractError::new(
                "ARTIFACT_QUOTE_DUPLICATE",
                format!(
                    "candidate contains duplicate Artifact quotes for {}",
                    quote.artifact.digest
                ),
            ));
        }
    }
    Ok(indexed)
}

fn artifact_ref_from_transfer_spec(
    spec: &core_v1::ArtifactTransferSpec,
) -> Result<ArtifactRef, FabricContractError> {
    let artifact = ArtifactRef {
        uri: spec.artifact_uri.clone(),
        digest: spec.digest.clone(),
        size_bytes: spec.size_bytes,
        kind: ArtifactKind::Generic,
        manifest_digest: (!spec.manifest_digest.is_empty()).then(|| spec.manifest_digest.clone()),
    };
    artifact.validate().map_err(|message| {
        FabricContractError::new(
            "ARTIFACT_ASSIGNMENT_IDENTITY_INVALID",
            format!("assignment Artifact transfer is invalid: {message}"),
        )
    })?;
    Ok(artifact)
}

fn semantic_lease_to_proto(lease: &semantic::Lease) -> semantic_v1::Lease {
    semantic_v1::Lease {
        identity: Some(identity_to_proto(&lease.identity)),
        holder: Some(identity_to_proto(&lease.holder)),
        resources: lease.resources.iter().map(identity_to_proto).collect(),
        state: match lease.state {
            semantic::LeaseState::Active => semantic_v1::LeaseState::Active,
            semantic::LeaseState::Releasing => semantic_v1::LeaseState::Releasing,
            semantic::LeaseState::Released => semantic_v1::LeaseState::Released,
            semantic::LeaseState::Expired => semantic_v1::LeaseState::Expired,
            semantic::LeaseState::Revoked => semantic_v1::LeaseState::Revoked,
            semantic::LeaseState::Failed => semantic_v1::LeaseState::Failed,
        } as i32,
        fence_token: lease.fence_token,
        expires_at: lease.expires_at_unix_ms.map(timestamp_from_unix_ms),
    }
}

fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn timestamp_from_unix_ms(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(unix_ms / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((unix_ms % 1000) * 1_000_000).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 10_000;

    fn identity(id: &str) -> semantic::Identity {
        semantic::Identity {
            id: id.to_string(),
            generation: 1,
        }
    }

    fn timestamp(unix_ms: u64) -> prost_types::Timestamp {
        timestamp_from_unix_ms(unix_ms)
    }

    fn builder() -> RuntimeAssignmentBuilder {
        builder_with_artifacts(Vec::new())
    }

    fn builder_with_artifacts(
        artifacts: Vec<core_v1::ArtifactTransferSpec>,
    ) -> RuntimeAssignmentBuilder {
        let runtime = identity("runtime-1");
        RuntimeAssignmentBuilder::new(
            "assignment-1",
            runtime.clone(),
            identity("operation-1"),
            "attempt-1",
            core_v1::WorkloadIdentity {
                identity: Some(semantic_v1::Identity {
                    id: "workload-1".to_string(),
                    generation: 1,
                }),
                scope: Some(core_v1::AccountScope {
                    user_id: "user-1".to_string(),
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                runtime: Some(core_v1::RuntimeRef {
                    identity: Some(identity_to_proto(&runtime)),
                }),
                allowed_actions: vec!["operation.report".to_string()],
                expires_at: Some(timestamp(30_000)),
            },
            core_v1::RuntimeProfile {
                image_digest: format!("sha256:{}", "1".repeat(64)),
                resolved_digest: format!("sha256:{}", "2".repeat(64)),
            },
            artifacts,
        )
    }

    fn artifact(seed: char, size_bytes: u64) -> ArtifactRef {
        let hex = seed.to_string().repeat(64);
        ArtifactRef {
            uri: format!("artifact://sha256/{hex}"),
            digest: format!("sha256:{hex}"),
            size_bytes,
            kind: ArtifactKind::Generic,
            manifest_digest: None,
        }
    }

    fn placement(artifacts: Vec<ArtifactRef>) -> ExecutionPlacementRequest {
        ExecutionPlacementRequest {
            capability_requirements: Vec::new(),
            resource_query: semantic::ResourceQuery {
                resource_class: "accelerator".to_string(),
                count: 1,
                required_capabilities: Vec::new(),
                minimum_capacity: BTreeMap::new(),
            },
            allowed_attachments: BTreeSet::new(),
            persistent: None,
            restart_capability: None,
            checkpoint_resume: false,
            network: Default::default(),
            artifacts,
            artifact_policy_scope: "policy-1".to_string(),
            policy: Default::default(),
            latest_start_unix_ms: None,
            now_unix_ms: NOW,
        }
    }

    fn candidate(
        artifact_quotes: Vec<ArtifactPlacementQuote>,
        artifact_destination_peer_id: &str,
    ) -> ExecutionTargetCandidate {
        let node = core_v1::NodeRef {
            node_id: "node-1".to_string(),
            node_epoch: 1,
        };
        let provider = identity("provider-1");
        ExecutionTargetCandidate {
            node,
            lifecycle_state: core_v1::NodeLifecycleState::Online,
            attachment: core_v1::ExecutionAttachmentType::ContainerAgent,
            persistent: false,
            restart_capability: core_v1::RestartCapability::None,
            capabilities: Vec::new(),
            provider: semantic::Provider {
                identity: provider.clone(),
                state: semantic::ProviderState::Ready,
                capabilities: Vec::new(),
            },
            provider_snapshot: semantic::ProviderSnapshot {
                provider,
                snapshot_generation: 1,
                resources: Vec::new(),
                workers: Vec::new(),
                endpoints: Vec::new(),
                sampled_at_unix_ms: NOW,
                expires_at_unix_ms: NOW + 1_000,
            },
            residency: "local".to_string(),
            trust_domain: "workspace".to_string(),
            classifications: BTreeSet::new(),
            policy_tags: BTreeSet::new(),
            artifact_destination_peer_id: artifact_destination_peer_id.to_string(),
            artifact_quotes,
            execution_cost_microunits: 1,
            available_at_unix_ms: NOW,
            reliability_score: 100,
        }
    }

    fn local_quote(artifact: &ArtifactRef, destination_peer_id: &str) -> ArtifactPlacementQuote {
        ArtifactPlacementQuote {
            quote_id: format!("local-{}", artifact.digest),
            artifact: artifact.clone(),
            destination_peer_id: destination_peer_id.to_string(),
            policy_scope: "policy-1".to_string(),
            observed_at_unix_ms: NOW,
            valid_until_unix_ms: NOW + 1_000,
            availability: ArtifactAvailability::VerifiedLocal {
                inventory_generation: 1,
            },
        }
    }

    fn transfer_quote(artifact: &ArtifactRef, destination_peer_id: &str) -> ArtifactPlacementQuote {
        ArtifactPlacementQuote {
            quote_id: format!("transfer-{}", artifact.digest),
            artifact: artifact.clone(),
            destination_peer_id: destination_peer_id.to_string(),
            policy_scope: "policy-1".to_string(),
            observed_at_unix_ms: NOW,
            valid_until_unix_ms: NOW + 1_000,
            availability: ArtifactAvailability::AuthorizedTransfer(
                crate::placement::ArtifactTransferQuote {
                    bytes_to_transfer: artifact.size_bytes,
                    estimated_transfer_millis: 1,
                    cost_microunits: 1,
                    source_count: 1,
                    authorization_valid_until_unix_ms: NOW + 1_000,
                },
            ),
        }
    }

    fn transfer_spec(
        artifact: &ArtifactRef,
        destination_peer_id: &str,
    ) -> core_v1::ArtifactTransferSpec {
        core_v1::ArtifactTransferSpec {
            artifact_uri: artifact.uri.clone(),
            digest: artifact.digest.clone(),
            size_bytes: artifact.size_bytes,
            manifest_digest: artifact.manifest_digest.clone().unwrap_or_default(),
            part_size_bytes: artifact.size_bytes.max(1),
            part_digests: if artifact.size_bytes == 0 {
                Vec::new()
            } else {
                vec![format!("sha256:{}", "9".repeat(64))]
            },
            sources: if artifact.size_bytes == 0 {
                Vec::new()
            } else {
                vec![core_v1::ArtifactTransferSource {
                    peer_id: "source-peer".to_string(),
                    replica_id: "replica-1".to_string(),
                    locator: "https://source.example.test/artifact".to_string(),
                    transfer_ticket: "opaque-ticket".to_string(),
                }]
            },
            part_sources: if artifact.size_bytes == 0 {
                Vec::new()
            } else {
                vec![core_v1::ArtifactPartSource {
                    part_index: 0,
                    peer_id: "source-peer".to_string(),
                    replica_id: "replica-1".to_string(),
                }]
            },
            destination_peer_id: destination_peer_id.to_string(),
            ..Default::default()
        }
    }

    fn lease(holder: semantic::Identity) -> semantic::Lease {
        semantic::Lease {
            identity: identity("lease-1"),
            holder,
            resources: vec![identity("resource-1")],
            state: semantic::LeaseState::Active,
            fence_token: 7,
            expires_at_unix_ms: Some(20_000),
        }
    }

    #[test]
    fn canonical_lease_is_projected_without_minting_authority_fields() {
        let builder = builder();
        let lease = lease(builder.runtime().clone());
        let assignment = builder.build(&lease, NOW).unwrap();
        let projected = assignment.lease.unwrap();

        assert_eq!(projected.identity.unwrap().id, lease.identity.id);
        assert_eq!(projected.holder.unwrap().id, lease.holder.id);
        assert_eq!(projected.resources[0].id, lease.resources[0].id);
        assert_eq!(projected.fence_token, lease.fence_token);
        assert_eq!(projected.expires_at, Some(timestamp(20_000)));
    }

    #[test]
    fn malformed_payload_fails_before_a_lease_is_needed() {
        let mut builder = builder();
        builder.attempt_id.clear();
        assert_eq!(
            builder.validate(NOW).unwrap_err().reason_code,
            "REQUIRED_FIELD_MISSING"
        );
    }

    #[test]
    fn wrong_holder_and_expired_authority_fail_closed() {
        let builder = builder();
        let wrong_holder = lease(identity("runtime-other"));
        assert_eq!(
            builder.build(&wrong_holder, NOW).unwrap_err().reason_code,
            "LEASE_HOLDER_MISMATCH"
        );

        let mut expired = lease(builder.runtime().clone());
        expired.expires_at_unix_ms = Some(NOW);
        assert_eq!(
            builder.build(&expired, NOW).unwrap_err().reason_code,
            "LEASE_EXPIRED"
        );
    }

    #[test]
    fn artifact_projection_rejects_canonical_identity_mismatch() {
        let artifact = artifact('a', 4);
        let mut spec = transfer_spec(&artifact, "peer-1");
        spec.size_bytes = 8;
        let result = builder_with_artifacts(vec![spec]).validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
        );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_ASSIGNMENT_IDENTITY_MISMATCH"
        );
    }

    #[test]
    fn artifact_projection_rejects_duplicate_assignment_artifacts() {
        let artifact = artifact('b', 4);
        let spec = transfer_spec(&artifact, "peer-1");
        let result = builder_with_artifacts(vec![spec.clone(), spec]).validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
        );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_ASSIGNMENT_DUPLICATE"
        );
    }

    #[test]
    fn artifact_projection_rejects_missing_transfer_artifact() {
        let artifact = artifact('c', 4);
        let result = builder().validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
        );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_ASSIGNMENT_MISSING"
        );
    }

    #[test]
    fn artifact_projection_rejects_transfer_destination_mismatch() {
        let artifact = artifact('d', 4);
        let result = builder_with_artifacts(vec![transfer_spec(&artifact, "peer-other")])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(
                    vec![transfer_quote(&artifact, "peer-selected")],
                    "peer-selected",
                ),
            );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_DESTINATION_MISMATCH"
        );
    }

    #[test]
    fn verified_local_artifact_fails_closed_without_an_identity_projection() {
        let artifact = artifact('e', 0);
        let result = builder().validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
        );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_LOCAL_INPUT_UNREPRESENTABLE"
        );
    }

    #[test]
    fn zero_byte_remote_artifact_fails_closed() {
        let artifact = artifact('f', 0);
        let result = builder_with_artifacts(vec![transfer_spec(&artifact, "peer-1")])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
            );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_ZERO_BYTE_TRANSFER_UNSUPPORTED"
        );
    }

    #[test]
    fn non_generic_remote_artifact_type_fails_closed() {
        let mut artifact = artifact('0', 4);
        artifact.kind = ArtifactKind::Model;
        let result = builder_with_artifacts(vec![transfer_spec(&artifact, "peer-1")])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
            );

        assert_eq!(
            result.unwrap_err().reason_code,
            "ARTIFACT_TYPE_UNREPRESENTABLE"
        );
    }
}
