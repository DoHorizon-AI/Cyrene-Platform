//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 placement.rs                                                    │
//! │  Module: cy_execution_fabric::placement                             │
//! │  Role: Provider-neutral execution target placement.                 │
//! │                                                                     │
//! │  模块职责：按资源、策略、数据移动成本和生命周期硬约束选择执行目标。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};

use cy_kernel_contract::{
    Capability as ContractCapability, CapabilityRequirement as ContractCapabilityRequirement,
    Identity, Provider, ProviderSnapshot, ProviderState, ResourceQuery, ResourceState,
};
use cy_manifest::ArtifactRef;
use cy_proto::core_v1::{ExecutionAttachmentType, NodeLifecycleState, NodeRef, RestartCapability};
use cy_proto::semantic_v1::{Capability, CapabilityRequirement};

use crate::{
    validate_execution_capability, FabricContractError, ARTIFACT_TRANSFER_CAPABILITY_ID,
    EXECUTION_CAPABILITY_ID,
};

/// Policy facts that every selected execution and Artifact Peer must satisfy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementPolicy {
    pub allowed_residencies: BTreeSet<String>,
    pub required_trust_domain: Option<String>,
    pub required_classifications: BTreeSet<String>,
    pub required_policy_tags: BTreeSet<String>,
    pub maximum_total_cost_microunits: Option<u64>,
}

/// Network behavior required by one Product-neutral execution request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkRequirements {
    pub outbound_https: bool,
    pub inbound_available: bool,
    pub peer_transfer: bool,
}

/// Product-neutral placement request compiled against canonical contracts.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlacementRequest {
    pub capability_requirements: Vec<CapabilityRequirement>,
    pub resource_query: ResourceQuery,
    pub allowed_attachments: BTreeSet<ExecutionAttachmentType>,
    pub persistent: Option<bool>,
    pub restart_capability: Option<RestartCapability>,
    pub checkpoint_resume: bool,
    pub network: NetworkRequirements,
    pub artifacts: Vec<ArtifactRef>,
    pub artifact_policy_scope: String,
    pub policy: PlacementPolicy,
    pub latest_start_unix_ms: Option<u64>,
    pub now_unix_ms: u64,
}

/// Transfer metrics derived by the Artifact Plane from an authorized plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactTransferQuote {
    pub bytes_to_transfer: u64,
    pub estimated_transfer_millis: u64,
    pub cost_microunits: u64,
    pub source_count: u32,
    pub authorization_valid_until_unix_ms: u64,
}

/// Artifact availability established outside the execution placement layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactAvailability {
    VerifiedLocal { inventory_generation: u64 },
    AuthorizedTransfer(ArtifactTransferQuote),
}

/// Time-bounded Artifact Plane evidence consumed by execution placement.
///
/// The quote does not expose replicas, tickets, or Peer-selection policy to
/// the Framework. Its policy scope must exactly match the placement request.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactPlacementQuote {
    pub quote_id: String,
    pub artifact: ArtifactRef,
    pub destination_peer_id: String,
    pub policy_scope: String,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub availability: ArtifactAvailability,
}

/// Scheduler input for one Node without cloud-vendor-specific behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionTargetCandidate {
    pub node: NodeRef,
    pub lifecycle_state: NodeLifecycleState,
    pub attachment: ExecutionAttachmentType,
    pub persistent: bool,
    pub restart_capability: RestartCapability,
    pub capabilities: Vec<Capability>,
    pub provider: Provider,
    pub provider_snapshot: ProviderSnapshot,
    pub residency: String,
    pub trust_domain: String,
    pub classifications: BTreeSet<String>,
    pub policy_tags: BTreeSet<String>,
    pub artifact_destination_peer_id: String,
    pub artifact_quotes: Vec<ArtifactPlacementQuote>,
    pub execution_cost_microunits: u64,
    pub available_at_unix_ms: u64,
    pub reliability_score: u32,
}

/// Stable, machine-readable explanation for an ineligible candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementReason {
    pub reason_code: &'static str,
    pub message: String,
}

impl PlacementReason {
    fn new(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            message: message.into(),
        }
    }
}

/// Read-only ProviderSnapshot evidence for one ResourceQuery.
///
/// This is not a reservation or allocation. Only the canonical Kernel Lease
/// authority can bind resources after placement selects a Node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceMatchEvidence {
    pub provider: Identity,
    pub snapshot_generation: u64,
    pub matched_resources: Vec<Identity>,
    pub required_count: u32,
}

/// Deterministic score derived only after every hard constraint passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementScore {
    pub local_artifact_bytes: u64,
    pub transfer_bytes: u64,
    pub estimated_transfer_millis: u64,
    pub total_cost_microunits: u64,
    pub estimated_ready_at_unix_ms: u64,
    pub reliability_score: u32,
}

/// Explainable eligibility result for one input candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateEvaluation {
    pub node: NodeRef,
    pub eligible: bool,
    pub reasons: Vec<PlacementReason>,
    pub resource_match: Option<ResourceMatchEvidence>,
    pub score: Option<PlacementScore>,
}

/// Complete placement result, including rejected candidates and selected Node.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementDecision {
    pub selected_node: Option<NodeRef>,
    pub evaluations: Vec<CandidateEvaluation>,
}

/// Evaluate every target, preserve rejection evidence, and select deterministically.
pub fn plan_execution_placement(
    request: &ExecutionPlacementRequest,
    candidates: &[ExecutionTargetCandidate],
) -> Result<PlacementDecision, FabricContractError> {
    validate_request(request)?;
    let mut node_identities = BTreeSet::new();
    for candidate in candidates {
        if !node_identities.insert((candidate.node.node_id.as_str(), candidate.node.node_epoch)) {
            return Err(FabricContractError::new(
                "EXECUTION_TARGET_DUPLICATE",
                "candidate set contains a duplicate NodeRef",
            ));
        }
    }
    let mut evaluations = candidates
        .iter()
        .map(|candidate| evaluate_candidate(request, candidate))
        .collect::<Vec<_>>();
    evaluations.sort_by(compare_evaluations);
    let selected_node = evaluations
        .iter()
        .find(|evaluation| evaluation.eligible)
        .map(|evaluation| evaluation.node.clone());
    Ok(PlacementDecision {
        selected_node,
        evaluations,
    })
}

/// Select one eligible target or fail closed when none satisfy the request.
pub fn place_execution_target<'a>(
    request: &ExecutionPlacementRequest,
    candidates: &'a [ExecutionTargetCandidate],
) -> Result<&'a ExecutionTargetCandidate, FabricContractError> {
    let decision = plan_execution_placement(request, candidates)?;
    let selected = decision.selected_node.ok_or_else(|| {
        FabricContractError::new(
            "WAITING_FOR_EXECUTION_TARGET",
            "no target satisfies the canonical resource, policy, lifecycle, and Artifact constraints",
        )
    })?;
    candidates
        .iter()
        .find(|candidate| candidate.node == selected)
        .ok_or_else(|| {
            FabricContractError::new(
                "PLACEMENT_DECISION_INVALID",
                "selected Node is absent from the evaluated candidate set",
            )
        })
}

fn validate_request(request: &ExecutionPlacementRequest) -> Result<(), FabricContractError> {
    if request.now_unix_ms == 0 {
        return Err(FabricContractError::new(
            "PLACEMENT_TIME_REQUIRED",
            "placement requires an explicit current timestamp",
        ));
    }
    if request
        .latest_start_unix_ms
        .is_some_and(|latest| latest < request.now_unix_ms)
    {
        return Err(FabricContractError::new(
            "PLACEMENT_DEADLINE_EXPIRED",
            "latest start time is earlier than the placement timestamp",
        ));
    }
    let mut capability_ids = BTreeSet::new();
    for requirement in &request.capability_requirements {
        let contract_requirement = ContractCapabilityRequirement {
            id: requirement.id.clone(),
            minimum_revision: requirement.minimum_revision,
            required_properties: requirement
                .required_properties
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        };
        if contract_requirement.validate().is_err()
            || !capability_ids.insert(requirement.id.as_str())
        {
            return Err(FabricContractError::new(
                "CAPABILITY_REQUIREMENT_INVALID",
                "capability requirements need unique identities and non-zero revisions",
            ));
        }
    }
    request.resource_query.validate().map_err(|error| {
        FabricContractError::new(
            "RESOURCE_QUERY_INVALID",
            format!("{}: {}", error.reason_code, error.message),
        )
    })?;
    if request
        .allowed_attachments
        .contains(&ExecutionAttachmentType::Unspecified)
        || request.restart_capability == Some(RestartCapability::Unspecified)
    {
        return Err(FabricContractError::new(
            "LIFECYCLE_REQUIREMENT_INVALID",
            "unspecified lifecycle values cannot be placement requirements",
        ));
    }
    if request
        .policy
        .required_trust_domain
        .as_ref()
        .is_some_and(String::is_empty)
        || request.policy.allowed_residencies.contains("")
        || request.policy.required_classifications.contains("")
        || request.policy.required_policy_tags.contains("")
    {
        return Err(FabricContractError::new(
            "PLACEMENT_POLICY_INVALID",
            "placement policy values must be non-empty",
        ));
    }
    let mut artifact_digests = BTreeSet::new();
    for artifact in &request.artifacts {
        artifact
            .validate()
            .map_err(|error| FabricContractError::new("ARTIFACT_REQUIREMENT_INVALID", error))?;
        if !artifact_digests.insert(artifact.digest.as_str()) {
            return Err(FabricContractError::new(
                "ARTIFACT_REQUIREMENT_DUPLICATE",
                "placement request contains a duplicate Artifact identity",
            ));
        }
    }
    if !request.artifacts.is_empty() && request.artifact_policy_scope.is_empty() {
        return Err(FabricContractError::new(
            "ARTIFACT_POLICY_SCOPE_REQUIRED",
            "Artifact placement requires an explicit Artifact Plane policy scope",
        ));
    }
    Ok(())
}

fn evaluate_candidate(
    request: &ExecutionPlacementRequest,
    candidate: &ExecutionTargetCandidate,
) -> CandidateEvaluation {
    let mut reasons = Vec::new();
    validate_candidate_identity(request, candidate, &mut reasons);
    validate_capabilities(request, candidate, &mut reasons);
    validate_policy(request, candidate, &mut reasons);

    let resource_match = match_resources(
        &request.resource_query,
        &candidate.provider_snapshot,
    )
    .or_else(|| {
        reasons.push(PlacementReason::new(
            "RESOURCE_REQUIREMENTS_UNSATISFIED",
            "ProviderSnapshot lacks enough distinct ready Resources for the canonical ResourceQuery",
        ));
        None
    });

    let artifact_score = evaluate_artifacts(request, candidate, &mut reasons);
    let total_cost = {
        candidate
            .execution_cost_microunits
            .checked_add(artifact_score.cost_microunits)
            .or_else(|| {
                reasons.push(PlacementReason::new(
                    "PLACEMENT_COST_OVERFLOW",
                    "execution and Artifact transfer cost overflowed",
                ));
                None
            })
    };
    if let (Some(maximum), Some(cost)) = (request.policy.maximum_total_cost_microunits, total_cost)
    {
        if cost > maximum {
            reasons.push(PlacementReason::new(
                "PLACEMENT_COST_EXCEEDED",
                format!("estimated cost {cost} exceeds policy maximum {maximum}"),
            ));
        }
    }
    let estimated_ready_at_unix_ms = request
        .now_unix_ms
        .max(candidate.available_at_unix_ms)
        .checked_add(artifact_score.estimated_transfer_millis)
        .or_else(|| {
            reasons.push(PlacementReason::new(
                "PLACEMENT_TIME_OVERFLOW",
                "candidate availability and Artifact transfer time overflowed",
            ));
            None
        });
    if let (Some(latest), Some(estimated_ready)) =
        (request.latest_start_unix_ms, estimated_ready_at_unix_ms)
    {
        if estimated_ready > latest {
            reasons.push(PlacementReason::new(
                "START_DEADLINE_UNSATISFIED",
                format!(
                    "estimated ready time {estimated_ready} exceeds latest start time {latest}"
                ),
            ));
        }
    }

    let score = if reasons.is_empty() {
        total_cost
            .zip(estimated_ready_at_unix_ms)
            .map(|(cost, estimated_ready_at_unix_ms)| PlacementScore {
                local_artifact_bytes: artifact_score.local_bytes,
                transfer_bytes: artifact_score.transfer_bytes,
                estimated_transfer_millis: artifact_score.estimated_transfer_millis,
                total_cost_microunits: cost,
                estimated_ready_at_unix_ms,
                reliability_score: candidate.reliability_score,
            })
    } else {
        None
    };
    CandidateEvaluation {
        node: candidate.node.clone(),
        eligible: score.is_some(),
        reasons,
        resource_match,
        score,
    }
}

fn validate_candidate_identity(
    request: &ExecutionPlacementRequest,
    candidate: &ExecutionTargetCandidate,
    reasons: &mut Vec<PlacementReason>,
) {
    if candidate.node.node_id.is_empty() || candidate.node.node_epoch == 0 {
        reasons.push(PlacementReason::new(
            "NODE_IDENTITY_INVALID",
            "candidate requires a NodeId and positive Node epoch",
        ));
    }
    if candidate.residency.is_empty() || candidate.trust_domain.is_empty() {
        reasons.push(PlacementReason::new(
            "EXECUTION_POLICY_FACTS_INVALID",
            "candidate execution residency and trust domain are required",
        ));
    }
    if candidate.available_at_unix_ms == 0 {
        reasons.push(PlacementReason::new(
            "AVAILABILITY_INVALID",
            "candidate availability timestamp is required",
        ));
    }
    if candidate.lifecycle_state != NodeLifecycleState::Online {
        reasons.push(PlacementReason::new(
            "NODE_NOT_ONLINE",
            "candidate Node is not online",
        ));
    }
    if candidate.attachment == ExecutionAttachmentType::Unspecified
        || candidate.restart_capability == RestartCapability::Unspecified
    {
        reasons.push(PlacementReason::new(
            "NODE_LIFECYCLE_INVALID",
            "candidate lifecycle facts must be explicit",
        ));
    }
    if !request.allowed_attachments.is_empty()
        && !request.allowed_attachments.contains(&candidate.attachment)
    {
        reasons.push(PlacementReason::new(
            "ATTACHMENT_NOT_ALLOWED",
            "candidate attachment type is not allowed",
        ));
    }
    if request
        .persistent
        .is_some_and(|value| value != candidate.persistent)
    {
        reasons.push(PlacementReason::new(
            "PERSISTENCE_REQUIREMENT_UNSATISFIED",
            "candidate persistence does not satisfy the request",
        ));
    }
    if request
        .restart_capability
        .is_some_and(|value| value != candidate.restart_capability)
    {
        reasons.push(PlacementReason::new(
            "RESTART_REQUIREMENT_UNSATISFIED",
            "candidate restart capability does not satisfy the request",
        ));
    }
    if candidate.provider.validate().is_err()
        || candidate.provider_snapshot.validate().is_err()
        || candidate.provider.identity != candidate.provider_snapshot.provider
    {
        reasons.push(PlacementReason::new(
            "PROVIDER_OBSERVATION_INVALID",
            "candidate Provider or ProviderSnapshot is invalid or mismatched",
        ));
    }
    if candidate.provider.state != ProviderState::Ready {
        reasons.push(PlacementReason::new(
            "PROVIDER_NOT_READY",
            "candidate Provider is not ready",
        ));
    }
    if candidate.provider_snapshot.sampled_at_unix_ms > request.now_unix_ms
        || candidate.provider_snapshot.expires_at_unix_ms <= request.now_unix_ms
    {
        reasons.push(PlacementReason::new(
            "PROVIDER_SNAPSHOT_STALE",
            "candidate ProviderSnapshot is not current at placement time",
        ));
    }
}

fn validate_capabilities(
    request: &ExecutionPlacementRequest,
    candidate: &ExecutionTargetCandidate,
    reasons: &mut Vec<PlacementReason>,
) {
    let mut capability_ids = BTreeSet::new();
    if candidate.capabilities.iter().any(|capability| {
        ContractCapability {
            id: capability.id.clone(),
            revision: capability.revision,
            properties: capability
                .properties
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
        .validate()
        .is_err()
            || !capability_ids.insert(capability.id.as_str())
    }) {
        reasons.push(PlacementReason::new(
            "CAPABILITY_OBSERVATION_INVALID",
            "candidate capabilities need unique identities, non-zero revisions, and non-empty properties",
        ));
    }
    if validate_execution_capability(
        &candidate.capabilities,
        candidate.attachment,
        candidate.persistent,
        candidate.restart_capability,
    )
    .is_err()
    {
        reasons.push(PlacementReason::new(
            "EXECUTION_CAPABILITY_INVALID",
            "typed lifecycle facts disagree with the canonical execution Capability",
        ));
    }
    if !request.capability_requirements.iter().all(|requirement| {
        candidate.capabilities.iter().any(|capability| {
            capability.id == requirement.id
                && capability.revision >= requirement.minimum_revision
                && requirement.required_properties.iter().all(|(key, value)| {
                    capability
                        .properties
                        .get(key)
                        .is_some_and(|actual| actual == value)
                })
        })
    }) {
        reasons.push(PlacementReason::new(
            "CAPABILITY_REQUIREMENTS_UNSATISFIED",
            "candidate does not satisfy every canonical Capability requirement",
        ));
    }
    let execution = candidate
        .capabilities
        .iter()
        .find(|capability| capability.id == EXECUTION_CAPABILITY_ID);
    let artifact_transfer_required = candidate.artifact_quotes.iter().any(|quote| {
        matches!(
            quote.availability,
            ArtifactAvailability::AuthorizedTransfer(_)
        )
    });
    let required_properties = [
        (request.checkpoint_resume, "checkpoint_resume"),
        (
            request.network.outbound_https || artifact_transfer_required,
            "outbound_https",
        ),
        (request.network.inbound_available, "inbound_available"),
        (
            request.network.peer_transfer || artifact_transfer_required,
            "peer_transfer",
        ),
    ];
    for (required, property) in required_properties {
        if required
            && execution.and_then(|value| value.properties.get(property).map(String::as_str))
                != Some("true")
        {
            reasons.push(PlacementReason::new(
                "EXECUTION_BEHAVIOR_UNSATISFIED",
                format!("candidate execution Capability lacks required {property}"),
            ));
        }
    }
    if artifact_transfer_required
        && !candidate.capabilities.iter().any(|capability| {
            capability.id == ARTIFACT_TRANSFER_CAPABILITY_ID
                && capability.revision >= 1
                && ["https_range", "multipart", "resume", "peer_transfer"]
                    .iter()
                    .all(|property| {
                        capability.properties.get(*property).map(String::as_str) == Some("true")
                    })
        })
    {
        reasons.push(PlacementReason::new(
            "ARTIFACT_TRANSFER_CAPABILITY_UNSATISFIED",
            "remote Artifact placement requires the canonical HTTPS range transfer Capability",
        ));
    }
}

fn validate_policy(
    request: &ExecutionPlacementRequest,
    candidate: &ExecutionTargetCandidate,
    reasons: &mut Vec<PlacementReason>,
) {
    if !policy_facts_match(
        &request.policy,
        &candidate.residency,
        &candidate.trust_domain,
        &candidate.classifications,
        &candidate.policy_tags,
    ) {
        reasons.push(PlacementReason::new(
            "EXECUTION_POLICY_DENIED",
            "candidate execution policy facts do not satisfy the request",
        ));
    }
    if !request.artifacts.is_empty()
        && (candidate.artifact_destination_peer_id.is_empty()
            || candidate.artifact_destination_peer_id == candidate.node.node_id)
    {
        reasons.push(PlacementReason::new(
            "ARTIFACT_DESTINATION_INVALID",
            "Artifact destination Peer must be explicit and distinct from Node identity",
        ));
    }
}

fn policy_facts_match(
    policy: &PlacementPolicy,
    residency: &str,
    trust_domain: &str,
    classifications: &BTreeSet<String>,
    policy_tags: &BTreeSet<String>,
) -> bool {
    (policy.allowed_residencies.is_empty() || policy.allowed_residencies.contains(residency))
        && policy
            .required_trust_domain
            .as_deref()
            .is_none_or(|required| required == trust_domain)
        && policy.required_classifications.is_subset(classifications)
        && policy.required_policy_tags.is_subset(policy_tags)
}

#[derive(Debug, Clone, Copy)]
struct ArtifactScore {
    local_bytes: u64,
    transfer_bytes: u64,
    estimated_transfer_millis: u64,
    cost_microunits: u64,
    source_count: u32,
}

fn evaluate_artifacts(
    request: &ExecutionPlacementRequest,
    candidate: &ExecutionTargetCandidate,
    reasons: &mut Vec<PlacementReason>,
) -> ArtifactScore {
    let mut quotes_by_digest = BTreeMap::<&str, &ArtifactPlacementQuote>::new();
    let mut quote_ids = BTreeSet::new();
    for quote in &candidate.artifact_quotes {
        if quote.quote_id.is_empty() || !quote_ids.insert(quote.quote_id.as_str()) {
            reasons.push(PlacementReason::new(
                "ARTIFACT_QUOTE_IDENTITY_INVALID",
                "Artifact placement quotes require unique non-empty identities",
            ));
        }
        if quotes_by_digest
            .insert(quote.artifact.digest.as_str(), quote)
            .is_some()
        {
            reasons.push(PlacementReason::new(
                "ARTIFACT_QUOTE_DUPLICATE",
                "candidate contains duplicate quotes for one Artifact",
            ));
        }
    }
    if quotes_by_digest.len() != request.artifacts.len() {
        reasons.push(PlacementReason::new(
            "ARTIFACT_QUOTE_SET_MISMATCH",
            "candidate Artifact quote set does not exactly match the request",
        ));
    }

    let mut score = ArtifactScore {
        local_bytes: 0,
        transfer_bytes: 0,
        estimated_transfer_millis: 0,
        cost_microunits: 0,
        source_count: 0,
    };
    let mut has_local_artifacts = false;
    let mut local_valid_until_unix_ms = u64::MAX;
    let mut transfer_valid_until_unix_ms = u64::MAX;
    for artifact in &request.artifacts {
        let Some(quote) = quotes_by_digest.get(artifact.digest.as_str()) else {
            reasons.push(PlacementReason::new(
                "ARTIFACT_QUOTE_MISSING",
                format!(
                    "candidate lacks an Artifact Plane quote for {}",
                    artifact.digest
                ),
            ));
            continue;
        };
        if quote.artifact != *artifact {
            reasons.push(PlacementReason::new(
                "ARTIFACT_IDENTITY_MISMATCH",
                "Artifact Plane quote disagrees with the requested identity",
            ));
            continue;
        }
        if quote.destination_peer_id != candidate.artifact_destination_peer_id
            || quote.policy_scope != request.artifact_policy_scope
        {
            reasons.push(PlacementReason::new(
                "ARTIFACT_QUOTE_SCOPE_MISMATCH",
                "Artifact quote destination or policy scope does not match placement",
            ));
            continue;
        }
        if quote.observed_at_unix_ms == 0
            || quote.observed_at_unix_ms > request.now_unix_ms
            || quote.valid_until_unix_ms <= request.now_unix_ms
        {
            reasons.push(PlacementReason::new(
                "ARTIFACT_QUOTE_STALE",
                "Artifact quote is not current at placement time",
            ));
            continue;
        }
        match quote.availability {
            ArtifactAvailability::VerifiedLocal {
                inventory_generation,
            } => {
                if inventory_generation == 0 {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_LOCALITY_EVIDENCE_INVALID",
                        "local Artifact evidence requires an inventory generation",
                    ));
                    continue;
                }
                if let Some(value) = score.local_bytes.checked_add(artifact.size_bytes) {
                    score.local_bytes = value;
                    has_local_artifacts = true;
                    local_valid_until_unix_ms =
                        local_valid_until_unix_ms.min(quote.valid_until_unix_ms);
                } else {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_ESTIMATE_OVERFLOW",
                        "local Artifact byte count overflowed",
                    ));
                }
            }
            ArtifactAvailability::AuthorizedTransfer(transfer) => {
                if transfer.bytes_to_transfer != artifact.size_bytes || transfer.source_count == 0 {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_TRANSFER_QUOTE_INVALID",
                        "Artifact transfer quote must cover the Artifact and use at least one source",
                    ));
                    continue;
                }
                let Some(cumulative_transfer_millis) = score
                    .estimated_transfer_millis
                    .checked_add(transfer.estimated_transfer_millis)
                else {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_ESTIMATE_OVERFLOW",
                        "aggregate Artifact transfer time overflowed",
                    ));
                    continue;
                };
                if quote.valid_until_unix_ms > transfer.authorization_valid_until_unix_ms {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_TRANSFER_AUTHORITY_EXPIRES",
                        "Artifact quote validity exceeds its underlying transfer authorization",
                    ));
                    continue;
                }
                let combined = score
                    .transfer_bytes
                    .checked_add(transfer.bytes_to_transfer)
                    .zip(score.cost_microunits.checked_add(transfer.cost_microunits))
                    .zip(score.source_count.checked_add(transfer.source_count));
                if let Some(((bytes, cost), sources)) = combined {
                    score.transfer_bytes = bytes;
                    score.estimated_transfer_millis = cumulative_transfer_millis;
                    score.cost_microunits = cost;
                    score.source_count = sources;
                    transfer_valid_until_unix_ms = transfer_valid_until_unix_ms
                        .min(quote.valid_until_unix_ms)
                        .min(transfer.authorization_valid_until_unix_ms);
                } else {
                    reasons.push(PlacementReason::new(
                        "ARTIFACT_ESTIMATE_OVERFLOW",
                        "aggregate Artifact transfer quote overflowed",
                    ));
                }
            }
        }
    }
    let artifact_ready_at_unix_ms = request
        .now_unix_ms
        .max(candidate.available_at_unix_ms)
        .checked_add(score.estimated_transfer_millis);
    match artifact_ready_at_unix_ms {
        None if has_local_artifacts || score.source_count > 0 => {
            reasons.push(PlacementReason::new(
                "ARTIFACT_ESTIMATE_OVERFLOW",
                "Artifact readiness time overflowed",
            ));
        }
        Some(ready) => {
            if has_local_artifacts && ready >= local_valid_until_unix_ms {
                reasons.push(PlacementReason::new(
                    "ARTIFACT_LOCALITY_EVIDENCE_EXPIRES",
                    "local Artifact inventory evidence expires before the candidate is ready",
                ));
            }
            if score.source_count > 0 && ready >= transfer_valid_until_unix_ms {
                reasons.push(PlacementReason::new(
                    "ARTIFACT_TRANSFER_AUTHORITY_EXPIRES",
                    "Artifact authorization or quote expires before all transfers can complete",
                ));
            }
        }
        None => {}
    }
    score
}

fn match_resources(
    query: &ResourceQuery,
    snapshot: &ProviderSnapshot,
) -> Option<ResourceMatchEvidence> {
    let mut matched = snapshot
        .resources
        .iter()
        .filter(|resource| resource.state == ResourceState::Ready && query.matches(resource))
        .map(|resource| resource.identity.clone())
        .collect::<Vec<_>>();
    matched.sort();
    let required = usize::try_from(query.count).ok()?;
    if matched.len() < required {
        return None;
    }
    Some(ResourceMatchEvidence {
        provider: snapshot.provider.clone(),
        snapshot_generation: snapshot.snapshot_generation,
        matched_resources: matched,
        required_count: query.count,
    })
}

fn compare_evaluations(
    left: &CandidateEvaluation,
    right: &CandidateEvaluation,
) -> std::cmp::Ordering {
    match (left.score, right.score) {
        (Some(left_score), Some(right_score)) => right_score
            .local_artifact_bytes
            .cmp(&left_score.local_artifact_bytes)
            .then_with(|| {
                left_score
                    .estimated_transfer_millis
                    .cmp(&right_score.estimated_transfer_millis)
            })
            .then_with(|| left_score.transfer_bytes.cmp(&right_score.transfer_bytes))
            .then_with(|| {
                left_score
                    .total_cost_microunits
                    .cmp(&right_score.total_cost_microunits)
            })
            .then_with(|| {
                left_score
                    .estimated_ready_at_unix_ms
                    .cmp(&right_score.estimated_ready_at_unix_ms)
            })
            .then_with(|| {
                right_score
                    .reliability_score
                    .cmp(&left_score.reliability_score)
            })
            .then_with(|| left.node.node_id.cmp(&right.node.node_id))
            .then_with(|| left.node.node_epoch.cmp(&right.node.node_epoch)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left
            .node
            .node_id
            .cmp(&right.node.node_id)
            .then_with(|| left.node.node_epoch.cmp(&right.node.node_epoch)),
    }
}
