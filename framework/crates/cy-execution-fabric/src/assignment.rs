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
/// 由 Product-neutral controller 提供的不可变非 authority field。Builder 不选择 target、不分配 resource，也不创建 Lease、fence、Runtime、Operation、Attempt 或 workload identity。调用方必须先持久化这些 identity，再调用规范 Kernel authority。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeAssignmentBuilder {
    assignment_id: String,
    runtime: semantic::Identity,
    operation: semantic::Identity,
    attempt_id: String,
    workload_identity: core_v1::WorkloadIdentity,
    profile: core_v1::RuntimeProfile,
    artifacts: Vec<core_v1::ArtifactTransferSpec>,
    local_artifacts: Vec<core_v1::ArtifactLocalInput>,
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
            local_artifacts: Vec::new(),
        }
    }

    /// Add immutable identity projections for Artifacts already verified on the
    /// selected Node. No local path, locator, ticket, or credential is accepted.
    /// 为已在所选 Node 上验证的 Artifact 添加不可变 identity projection。不接受本地 path、locator、ticket 或 credential。
    pub fn with_local_artifacts(
        mut self,
        local_artifacts: Vec<core_v1::ArtifactLocalInput>,
    ) -> Self {
        self.local_artifacts = local_artifacts;
        self
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
    /// candidate. Local inputs and transfer specs must form an exact, disjoint
    /// projection of the placement request; replica selection and ticket
    /// issuance remain outside Framework.
    /// Placement 选择 candidate 后，校验不可变 Artifact projection。本地 input 与 transfer spec 必须精确且互不重叠地覆盖 placement request；Replica 选择与 ticket 签发都留在 Framework 之外。
    ///
    /// Zero-byte local Artifacts remain valid. Remote transfers require a
    /// positive size and preserve the producer-owned opaque category.
    /// 零字节本地 Artifact 仍有效。远端 transfer 要求 size 为正，并保留由 producer 所有的不透明 category。
    pub fn validate_artifact_projection(
        &self,
        placement: &ExecutionPlacementRequest,
        selected: &ExecutionTargetCandidate,
    ) -> Result<(), FabricContractError> {
        let requested = index_requested_artifacts(&placement.artifacts)?;
        let assignments = index_assignment_artifacts(&self.artifacts)?;
        let local_artifacts = index_local_artifacts(&self.local_artifacts)?;
        let quotes = index_artifact_quotes(&selected.artifact_quotes)?;

        if let Some(digest) = assignments
            .keys()
            .find(|digest| local_artifacts.contains_key(*digest))
        {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_DUPLICATE",
                format!("assignment projects Artifact {digest} as both local and transfer"),
            ));
        }

        let requested_digests = requested.keys().cloned().collect::<BTreeSet<_>>();
        let projection_digests = assignments
            .keys()
            .chain(local_artifacts.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(digest) = projection_digests.difference(&requested_digests).next() {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_UNEXPECTED",
                format!("assignment contains an Artifact absent from placement: {digest}"),
            ));
        }
        if quotes.len() != requested.len() {
            return Err(FabricContractError::new(
                "ARTIFACT_QUOTE_SET_MISMATCH",
                "selected candidate Artifact quote set does not exactly match placement",
            ));
        }

        let destination = &selected.artifact_destination_peer_id;
        if !requested.is_empty()
            && (destination.is_empty() || destination == &selected.node.node_id)
        {
            return Err(FabricContractError::new(
                "ARTIFACT_DESTINATION_INVALID",
                "selected candidate requires an explicit Artifact Peer distinct from Node identity",
            ));
        }

        if let Some(digest) = requested_digests.difference(&projection_digests).next() {
            let reason_code = match quotes.get(digest).map(|quote| &quote.availability) {
                Some(ArtifactAvailability::VerifiedLocal { .. }) => "ARTIFACT_LOCAL_INPUT_MISSING",
                _ => "ARTIFACT_ASSIGNMENT_MISSING",
            };
            return Err(FabricContractError::new(
                reason_code,
                format!("assignment lacks the Artifact projection for {digest}"),
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
                    let local = local_artifacts.get(&digest).ok_or_else(|| {
                        FabricContractError::new(
                            "ARTIFACT_LOCAL_INPUT_MISSING",
                            format!("assignment lacks the local projection for Artifact {digest}"),
                        )
                    })?;
                    let projected = artifact_ref_from_local_input(local)?;
                    if projected != *artifact {
                        return Err(FabricContractError::new(
                            "ARTIFACT_LOCAL_INPUT_IDENTITY_MISMATCH",
                            format!(
                                "assignment local input disagrees with placement identity for {digest}"
                            ),
                        ));
                    }
                }
                ArtifactAvailability::AuthorizedTransfer(_) => {
                    if artifact.size_bytes == 0 {
                        return Err(FabricContractError::new(
                            "ARTIFACT_ZERO_BYTE_TRANSFER_UNSUPPORTED",
                            format!("zero-byte remote Artifact cannot be projected for transfer: {digest}"),
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
    /// 校验所有不需要 Lease authority 的 assignment field。Controller 在调用 acquire_lease 前执行此项，因此格式错误的 payload 不会消耗 resource，也不会依赖等待 expiry 来清理。
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), FabricContractError> {
        let assignment = self.assignment(None);
        validate_assignment_payload(&self.runtime, &assignment, now_unix_ms).map(|_| ())
    }

    /// Bind an authority-returned Lease and run the same admission validation
    /// that the Runtime Agent applies before starting its workload.
    /// 绑定 authority 返回的 Lease，并执行与 Runtime Agent 启动 workload 前相同的 admission validation。
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
            local_artifacts: self.local_artifacts.clone(),
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

fn index_local_artifacts(
    artifacts: &[core_v1::ArtifactLocalInput],
) -> Result<BTreeMap<String, &core_v1::ArtifactLocalInput>, FabricContractError> {
    let mut indexed = BTreeMap::new();
    for input in artifacts {
        let projected = artifact_ref_from_local_input(input)?;
        if indexed.insert(projected.digest.clone(), input).is_some() {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_DUPLICATE",
                format!(
                    "assignment contains a duplicate local Artifact digest: {}",
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

#[cfg(test)]
fn artifact_kind_to_proto(kind: &ArtifactKind) -> &str {
    kind.as_str()
}

pub(crate) fn artifact_kind_from_proto(value: &str) -> Result<ArtifactKind, FabricContractError> {
    ArtifactKind::new(value).map_err(|message| {
        FabricContractError::new(
            "ARTIFACT_KIND_INVALID",
            format!("Artifact kind is invalid: {message}"),
        )
    })
}

fn artifact_ref_from_local_input(
    input: &core_v1::ArtifactLocalInput,
) -> Result<ArtifactRef, FabricContractError> {
    let kind = artifact_kind_from_proto(&input.artifact_kind)?;
    let artifact = ArtifactRef {
        uri: input.artifact_uri.clone(),
        digest: input.digest.clone(),
        size_bytes: input.size_bytes,
        kind,
        manifest_digest: (!input.manifest_digest.is_empty()).then(|| input.manifest_digest.clone()),
    };
    artifact.validate().map_err(|message| {
        FabricContractError::new(
            "ARTIFACT_LOCAL_IDENTITY_INVALID",
            format!("assignment local Artifact is invalid: {message}"),
        )
    })?;
    Ok(artifact)
}

fn artifact_ref_from_transfer_spec(
    spec: &core_v1::ArtifactTransferSpec,
) -> Result<ArtifactRef, FabricContractError> {
    let artifact = ArtifactRef {
        uri: spec.artifact_uri.clone(),
        digest: spec.digest.clone(),
        size_bytes: spec.size_bytes,
        kind: artifact_kind_from_proto(&spec.artifact_kind)?,
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
            kind: ArtifactKind::generic(),
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
            artifact_kind: artifact.kind.as_str().to_string(),
            ..Default::default()
        }
    }

    fn local_input(artifact: &ArtifactRef) -> core_v1::ArtifactLocalInput {
        core_v1::ArtifactLocalInput {
            artifact_uri: artifact.uri.clone(),
            digest: artifact.digest.clone(),
            size_bytes: artifact.size_bytes,
            artifact_kind: artifact_kind_to_proto(&artifact.kind).to_string(),
            manifest_digest: artifact.manifest_digest.clone().unwrap_or_default(),
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
    fn verified_local_artifact_preserves_identity_without_transfer_authority() {
        let artifact = artifact('e', 0);
        let builder = builder().with_local_artifacts(vec![local_input(&artifact)]);
        let result = builder.validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
        );

        assert_eq!(result, Ok(()));
        let assignment = builder
            .build(&lease(builder.runtime().clone()), NOW)
            .unwrap();
        assert!(assignment.artifacts.is_empty());
        assert_eq!(assignment.local_artifacts, vec![local_input(&artifact)]);
    }

    #[test]
    fn verified_local_artifact_requires_an_exact_local_projection() {
        let artifact = artifact('e', 4);
        let missing = builder().validate_artifact_projection(
            &placement(vec![artifact.clone()]),
            &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
        );
        assert_eq!(
            missing.unwrap_err().reason_code,
            "ARTIFACT_LOCAL_INPUT_MISSING"
        );

        let mut mismatched = local_input(&artifact);
        mismatched.size_bytes += 1;
        let mismatch = builder()
            .with_local_artifacts(vec![mismatched])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
            );
        assert_eq!(
            mismatch.unwrap_err().reason_code,
            "ARTIFACT_LOCAL_INPUT_IDENTITY_MISMATCH"
        );
    }

    #[test]
    fn local_projection_rejects_bad_kind_and_cross_projection_duplicates() {
        let artifact = artifact('e', 4);
        let mut bad_kind = local_input(&artifact);
        bad_kind.artifact_kind = "bad\nkind".to_string();
        let bad_kind_result = builder()
            .with_local_artifacts(vec![bad_kind])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
            );
        assert_eq!(
            bad_kind_result.unwrap_err().reason_code,
            "ARTIFACT_KIND_INVALID"
        );

        let duplicate = builder_with_artifacts(vec![transfer_spec(&artifact, "peer-1")])
            .with_local_artifacts(vec![local_input(&artifact)])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![local_quote(&artifact, "peer-1")], "peer-1"),
            );
        assert_eq!(
            duplicate.unwrap_err().reason_code,
            "ARTIFACT_ASSIGNMENT_DUPLICATE"
        );
    }

    #[test]
    fn producer_owned_artifact_kinds_round_trip_through_local_projection() {
        for kind in [
            ArtifactKind::generic(),
            ArtifactKind::new("product.snapshot.v2").unwrap(),
        ] {
            assert_eq!(
                artifact_kind_from_proto(artifact_kind_to_proto(&kind)),
                Ok(kind.clone())
            );
        }
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
    fn producer_owned_remote_artifact_type_is_preserved() {
        let mut artifact = artifact('0', 4);
        artifact.kind = ArtifactKind::new("product.snapshot.v2").unwrap();
        let result = builder_with_artifacts(vec![transfer_spec(&artifact, "peer-1")])
            .validate_artifact_projection(
                &placement(vec![artifact.clone()]),
                &candidate(vec![transfer_quote(&artifact, "peer-1")], "peer-1"),
            );

        result.expect("opaque Artifact categories must survive remote assignment");
    }
}
