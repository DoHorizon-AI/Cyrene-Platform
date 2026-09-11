//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 admission.rs                                                    │
//! │  Module: cy_execution_fabric::admission                             │
//! │  Role: Validate Runtime assignments against canonical authority.    │
//! │                                                                     │
//! │  模块职责：按 Runtime generation 与 Kernel Lease/Fence 校验准入。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeSet, VecDeque};

use cy_kernel_contract as semantic;
use cy_proto::{core_v1, semantic_v1};
use thiserror::Error;

const MAX_REPLAY_IDS: usize = 1024;

/// Stable rejection returned by the Fabric projection.
///
/// Fabric 拒绝保留稳定 reason code，便于跨语言客户端处理。
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{reason_code}: {message}")]
pub struct FabricContractError {
    pub reason_code: &'static str,
    pub message: String,
}

impl FabricContractError {
    pub(crate) fn new(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            message: message.into(),
        }
    }
}

/// Result of admitting an idempotent sequenced observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDisposition {
    Accepted,
    Duplicate,
}

/// Bounded sequence and idempotency cursor for one authenticated session.
///
/// 每个已认证 session 独立维护连续序号与有限去重窗口。
#[derive(Debug, Default)]
pub struct ObservationCursor {
    last_sequence: u64,
    recent_ids: BTreeSet<String>,
    order: VecDeque<String>,
}

impl ObservationCursor {
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Admit a new frame or recognize an identical replay.
    pub fn admit(
        &mut self,
        frame_id: &str,
        sequence: u64,
    ) -> Result<AdmissionDisposition, FabricContractError> {
        if frame_id.is_empty() || sequence == 0 {
            return Err(FabricContractError::new(
                "FRAME_INVALID",
                "frame id and non-zero sequence are required",
            ));
        }
        if self.recent_ids.contains(frame_id) {
            return Ok(AdmissionDisposition::Duplicate);
        }
        if sequence != self.last_sequence + 1 {
            return Err(FabricContractError::new(
                "SEQUENCE_GAP",
                format!(
                    "expected sequence {}, received {sequence}",
                    self.last_sequence + 1
                ),
            ));
        }
        self.last_sequence = sequence;
        let owned_id = frame_id.to_string();
        self.recent_ids.insert(owned_id.clone());
        self.order.push_back(owned_id);
        if self.order.len() > MAX_REPLAY_IDS {
            if let Some(evicted) = self.order.pop_front() {
                self.recent_ids.remove(&evicted);
            }
        }
        Ok(AdmissionDisposition::Accepted)
    }
}

/// Validate an Execution Agent hello without persisting its bootstrap proof.
pub fn validate_hello(hello: &core_v1::ExecutionAgentHello) -> Result<(), FabricContractError> {
    validate_runtime(hello.runtime.as_ref())?;
    let node = hello.node.as_ref().ok_or_else(|| {
        FabricContractError::new("NODE_IDENTITY_REQUIRED", "Node descriptor is required")
    })?;
    crate::node::validate_node_descriptor(node)?;
    let attachment = core_v1::ExecutionAttachmentType::try_from(hello.attachment_type)
        .map_err(|_| FabricContractError::new("UNKNOWN_ENUM_VALUE", "unknown attachment type"))?;
    if attachment == core_v1::ExecutionAttachmentType::Unspecified {
        return Err(FabricContractError::new(
            "REQUIRED_FIELD_MISSING",
            "attachment type is required",
        ));
    }
    let persistence = core_v1::PersistenceClass::try_from(hello.persistence_class)
        .map_err(|_| FabricContractError::new("UNKNOWN_ENUM_VALUE", "unknown persistence class"))?;
    if persistence == core_v1::PersistenceClass::Unspecified {
        return Err(FabricContractError::new(
            "REQUIRED_FIELD_MISSING",
            "persistence class is required",
        ));
    }
    let persistent = node.persistent.expect("validated Node persistence");
    let expected_persistence = if persistent {
        core_v1::PersistenceClass::Persistent
    } else {
        core_v1::PersistenceClass::Ephemeral
    };
    if persistence != expected_persistence {
        return Err(FabricContractError::new(
            "NODE_PERSISTENCE_MISMATCH",
            "persistent disagrees with the compatibility persistence_class",
        ));
    }
    let restart = core_v1::RestartCapability::try_from(hello.restart_capability).map_err(|_| {
        FabricContractError::new("UNKNOWN_ENUM_VALUE", "unknown restart capability")
    })?;
    let restart_matches_attachment = matches!(
        (attachment, restart),
        (
            core_v1::ExecutionAttachmentType::ContainerAgent,
            core_v1::RestartCapability::None
        ) | (
            core_v1::ExecutionAttachmentType::HostAgent,
            core_v1::RestartCapability::HostSupervised
        ) | (
            core_v1::ExecutionAttachmentType::ProviderManaged,
            core_v1::RestartCapability::ProviderSupervised
        )
    );
    if !restart_matches_attachment {
        return Err(FabricContractError::new(
            "RESTART_CAPABILITY_INVALID",
            "restart capability is not provided by this attachment type",
        ));
    }
    if hello.min_protocol_version == 0
        || hello.min_protocol_version > 2
        || hello.max_protocol_version < 2
        || hello.agent_version.is_empty()
    {
        return Err(FabricContractError::new(
            "PROTOCOL_INCOMPATIBLE",
            "Execution Agent protocol range must include version 2",
        ));
    }
    if hello.enrollment_proof.is_empty() && hello.resume_token.is_empty() {
        return Err(FabricContractError::new(
            "AUTHENTICATION_REQUIRED",
            "an enrollment proof or authenticated resume token is required",
        ));
    }
    if !hello.enrollment_proof.is_empty() && !hello.resume_token.is_empty() {
        return Err(FabricContractError::new(
            "CREDENTIAL_AMBIGUOUS",
            "enrollment proof and resume token cannot be presented together",
        ));
    }
    if hello
        .scope
        .as_ref()
        .is_none_or(|scope| scope.workspace_id.is_empty())
    {
        return Err(FabricContractError::new(
            "WORKSPACE_REQUIRED",
            "workspace identity is required",
        ));
    }
    crate::capability::validate_execution_capability(
        &hello.capabilities,
        attachment,
        persistent,
        restart,
    )?;
    Ok(())
}

/// Validate that an Assignment is current, immutable, and Lease-authorized.
pub fn validate_assignment(
    expected_runtime: &semantic::Identity,
    assignment: &core_v1::RuntimeAssignment,
    now_unix_ms: u64,
) -> Result<semantic::Lease, FabricContractError> {
    let runtime = validate_assignment_payload(expected_runtime, assignment, now_unix_ms)?;
    let lease = lease_from_proto(assignment.lease.as_ref())?;
    lease.validate().map_err(contract_error)?;
    if lease.state != semantic::LeaseState::Active {
        return Err(FabricContractError::new(
            "LEASE_NOT_ACTIVE",
            "assignment Lease is not active",
        ));
    }
    if lease.holder != runtime {
        return Err(FabricContractError::new(
            "LEASE_HOLDER_MISMATCH",
            "assignment Lease holder is not the Runtime generation",
        ));
    }
    if lease
        .expires_at_unix_ms
        .is_none_or(|expiry| now_unix_ms >= expiry)
    {
        return Err(FabricContractError::new(
            "LEASE_EXPIRED",
            "assignment Lease has expired",
        ));
    }
    Ok(lease)
}

/// Validate every non-authority field before asking the Kernel to issue a
/// Lease. This prevents malformed assignment payloads from consuming scarce
/// resources and deliberately ignores only `assignment.lease`.
pub(crate) fn validate_assignment_payload(
    expected_runtime: &semantic::Identity,
    assignment: &core_v1::RuntimeAssignment,
    now_unix_ms: u64,
) -> Result<semantic::Identity, FabricContractError> {
    if assignment.assignment_id.is_empty() || assignment.attempt_id.is_empty() {
        return Err(FabricContractError::new(
            "REQUIRED_FIELD_MISSING",
            "assignment and Attempt identities are required",
        ));
    }
    let runtime = runtime_identity(assignment.runtime.as_ref())?;
    if runtime.id != expected_runtime.id || runtime.generation < expected_runtime.generation {
        return Err(FabricContractError::new(
            "STALE_GENERATION",
            "assignment Runtime generation is stale or belongs to another Runtime",
        ));
    }
    if &runtime != expected_runtime {
        return Err(FabricContractError::new(
            "GENERATION_MISMATCH",
            "assignment Runtime generation does not match this Agent",
        ));
    }
    validate_identity(assignment.operation.as_ref(), "operation")?;
    if core_v1::DesiredRuntimeState::try_from(assignment.desired_state)
        .ok()
        .filter(|state| *state == core_v1::DesiredRuntimeState::Running)
        .is_none()
    {
        return Err(FabricContractError::new(
            "DESIRED_STATE_INVALID",
            "a new Assignment must desire RUNNING",
        ));
    }

    let identity = assignment.workload_identity.as_ref().ok_or_else(|| {
        FabricContractError::new("REQUIRED_FIELD_MISSING", "workload identity is required")
    })?;
    validate_identity(identity.identity.as_ref(), "workload identity")?;
    let identity_runtime = runtime_identity(identity.runtime.as_ref())?;
    if identity_runtime != runtime {
        return Err(FabricContractError::new(
            "WORKLOAD_IDENTITY_SCOPE_MISMATCH",
            "workload identity is scoped to another Runtime generation",
        ));
    }
    if timestamp_ms(identity.expires_at.as_ref())?.is_none_or(|expiry| now_unix_ms >= expiry) {
        return Err(FabricContractError::new(
            "WORKLOAD_IDENTITY_EXPIRED",
            "workload identity is expired",
        ));
    }

    let profile = assignment.profile.as_ref().ok_or_else(|| {
        FabricContractError::new("REQUIRED_FIELD_MISSING", "Runtime profile is required")
    })?;
    validate_sha256(&profile.image_digest, "image digest")?;
    validate_sha256(&profile.resolved_digest, "resolved runtime digest")?;
    let mut artifact_digests = BTreeSet::new();
    for artifact in &assignment.artifacts {
        validate_sha256(&artifact.digest, "Artifact digest")?;
        if !artifact_digests.insert(artifact.digest.clone()) {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_DUPLICATE",
                format!(
                    "assignment contains a duplicate Artifact digest: {}",
                    artifact.digest
                ),
            ));
        }
        if artifact.artifact_uri != format!("artifact://sha256/{}", &artifact.digest[7..])
            || artifact.size_bytes == 0
            || artifact.part_size_bytes == 0
            || artifact.sources.is_empty()
        {
            return Err(FabricContractError::new(
                "ARTIFACT_TRANSFER_INVALID",
                "Artifact transfer identity, size, part size, and replica are required",
            ));
        }
        if !artifact.manifest_digest.is_empty() {
            validate_sha256(&artifact.manifest_digest, "Artifact manifest digest")?;
        }
        crate::assignment::artifact_kind_from_proto(&artifact.artifact_kind)?;
        if !artifact.sources.is_empty() {
            if artifact.destination_peer_id.is_empty()
                || artifact.part_sources.len() != artifact.part_digests.len()
                || artifact.sources.iter().any(|source| {
                    source.peer_id.is_empty()
                        || source.replica_id.is_empty()
                        || !source.locator.starts_with("https://")
                        || source.transfer_ticket.is_empty()
                })
            {
                return Err(FabricContractError::new(
                    "ARTIFACT_TRANSFER_PLAN_INVALID",
                    "source Peers, destination Peer, tickets, and per-part routes are required",
                ));
            }
            let expected_parts = artifact.part_digests.len();
            let routed = artifact
                .part_sources
                .iter()
                .map(|route| route.part_index)
                .collect::<BTreeSet<_>>();
            if routed.len() != expected_parts
                || routed
                    .iter()
                    .enumerate()
                    .any(|(index, part)| usize::try_from(*part).ok() != Some(index))
            {
                return Err(FabricContractError::new(
                    "ARTIFACT_PART_ROUTING_INVALID",
                    "every Artifact part must have exactly one source route",
                ));
            }
        }
    }
    for artifact in &assignment.local_artifacts {
        validate_sha256(&artifact.digest, "local Artifact digest")?;
        if !artifact_digests.insert(artifact.digest.clone()) {
            return Err(FabricContractError::new(
                "ARTIFACT_ASSIGNMENT_DUPLICATE",
                format!(
                    "assignment contains a duplicate Artifact digest: {}",
                    artifact.digest
                ),
            ));
        }
        if artifact.artifact_uri != format!("artifact://sha256/{}", &artifact.digest[7..]) {
            return Err(FabricContractError::new(
                "ARTIFACT_LOCAL_IDENTITY_INVALID",
                "local Artifact URI must match its canonical digest",
            ));
        }
        crate::assignment::artifact_kind_from_proto(&artifact.artifact_kind)?;
        if !artifact.manifest_digest.is_empty() {
            validate_sha256(&artifact.manifest_digest, "local Artifact manifest digest")?;
        }
    }
    Ok(runtime)
}

/// Validate a renewed Lease using the canonical Lease transition rules.
pub fn validate_renewal(
    previous: &semantic::Lease,
    renewed: &semantic_v1::Lease,
    now_unix_ms: u64,
) -> Result<semantic::Lease, FabricContractError> {
    let renewed = lease_from_proto(Some(renewed))?;
    if renewed.identity != previous.identity
        || renewed.holder != previous.holder
        || renewed.resources != previous.resources
        || renewed.fence_token != previous.fence_token
    {
        return Err(FabricContractError::new(
            "LEASE_RENEWAL_INVALID",
            "renewal changed immutable Lease authority",
        ));
    }
    let expected = previous
        .renew_lease(
            renewed.fence_token,
            renewed.expires_at_unix_ms.unwrap_or_default(),
            now_unix_ms,
        )
        .map_err(contract_error)?;
    if renewed != expected {
        return Err(FabricContractError::new(
            "LEASE_RENEWAL_INVALID",
            "renewed Lease does not match the canonical transition",
        ));
    }
    Ok(renewed)
}

/// Decode the canonical semantic Lease projection without granting it any
/// authority beyond the signed/fenced fields present on the wire.
pub fn semantic_lease_from_proto(
    lease: &semantic_v1::Lease,
) -> Result<semantic::Lease, FabricContractError> {
    let lease = lease_from_proto(Some(lease))?;
    lease.validate().map_err(contract_error)?;
    Ok(lease)
}

fn validate_runtime(runtime: Option<&core_v1::RuntimeRef>) -> Result<(), FabricContractError> {
    runtime_identity(runtime).map(|_| ())
}

fn runtime_identity(
    runtime: Option<&core_v1::RuntimeRef>,
) -> Result<semantic::Identity, FabricContractError> {
    let runtime = runtime.ok_or_else(|| {
        FabricContractError::new("REQUIRED_FIELD_MISSING", "Runtime identity is required")
    })?;
    validate_identity(runtime.identity.as_ref(), "Runtime")
}

fn validate_identity(
    identity: Option<&semantic_v1::Identity>,
    name: &str,
) -> Result<semantic::Identity, FabricContractError> {
    let identity = identity.ok_or_else(|| {
        FabricContractError::new(
            "REQUIRED_FIELD_MISSING",
            format!("{name} identity is required"),
        )
    })?;
    let identity = semantic::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    };
    identity.validate().map_err(contract_error)?;
    Ok(identity)
}

fn lease_from_proto(
    lease: Option<&semantic_v1::Lease>,
) -> Result<semantic::Lease, FabricContractError> {
    let lease = lease
        .ok_or_else(|| FabricContractError::new("REQUIRED_FIELD_MISSING", "Lease is required"))?;
    let state = match semantic_v1::LeaseState::try_from(lease.state) {
        Ok(semantic_v1::LeaseState::Active) => semantic::LeaseState::Active,
        Ok(semantic_v1::LeaseState::Releasing) => semantic::LeaseState::Releasing,
        Ok(semantic_v1::LeaseState::Released) => semantic::LeaseState::Released,
        Ok(semantic_v1::LeaseState::Expired) => semantic::LeaseState::Expired,
        Ok(semantic_v1::LeaseState::Revoked) => semantic::LeaseState::Revoked,
        Ok(semantic_v1::LeaseState::Failed) => semantic::LeaseState::Failed,
        _ => {
            return Err(FabricContractError::new(
                "UNKNOWN_ENUM_VALUE",
                "Lease state is invalid",
            ));
        }
    };
    Ok(semantic::Lease {
        identity: validate_identity(lease.identity.as_ref(), "Lease")?,
        holder: validate_identity(lease.holder.as_ref(), "Lease holder")?,
        resources: lease
            .resources
            .iter()
            .map(|identity| validate_identity(Some(identity), "Lease resource"))
            .collect::<Result<Vec<_>, _>>()?,
        state,
        fence_token: lease.fence_token,
        expires_at_unix_ms: timestamp_ms(lease.expires_at.as_ref())?,
    })
}

fn timestamp_ms(
    timestamp: Option<&prost_types::Timestamp>,
) -> Result<Option<u64>, FabricContractError> {
    let Some(timestamp) = timestamp else {
        return Ok(None);
    };
    if timestamp.seconds <= 0 || timestamp.nanos < 0 || timestamp.nanos % 1_000_000 != 0 {
        return Err(FabricContractError::new(
            "TIMESTAMP_INVALID",
            "timestamp must be a positive exact Unix millisecond",
        ));
    }
    let seconds = u64::try_from(timestamp.seconds).map_err(|_| {
        FabricContractError::new("TIMESTAMP_INVALID", "timestamp seconds are out of range")
    })?;
    let millis = seconds
        .checked_mul(1000)
        .and_then(|value| value.checked_add(u64::try_from(timestamp.nanos / 1_000_000).ok()?))
        .ok_or_else(|| {
            FabricContractError::new("TIMESTAMP_INVALID", "timestamp is out of range")
        })?;
    Ok(Some(millis))
}

fn validate_sha256(value: &str, name: &str) -> Result<(), FabricContractError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if !valid {
        return Err(FabricContractError::new(
            "DIGEST_INVALID",
            format!("{name} must be a lowercase SHA-256 digest"),
        ));
    }
    Ok(())
}

fn contract_error(error: semantic::ContractError) -> FabricContractError {
    FabricContractError::new(
        match error.reason_code {
            "GENERATION_INVALID" => "GENERATION_INVALID",
            "FENCE_TOKEN_INVALID" => "FENCE_TOKEN_INVALID",
            "LEASE_EXPIRED" => "LEASE_EXPIRED",
            "LEASE_NOT_ACTIVE" => "LEASE_NOT_ACTIVE",
            "LEASE_RENEWAL_INVALID" => "LEASE_RENEWAL_INVALID",
            "FENCE_MISMATCH" => "FENCE_MISMATCH",
            _ => "SEMANTIC_CONTRACT_REJECTED",
        },
        error.message,
    )
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    fn timestamp(value: u64) -> prost_types::Timestamp {
        prost_types::Timestamp {
            seconds: i64::try_from(value / 1000).unwrap(),
            nanos: i32::try_from((value % 1000) * 1_000_000).unwrap(),
        }
    }

    fn identity(id: &str, generation: u64) -> semantic_v1::Identity {
        semantic_v1::Identity {
            id: id.to_string(),
            generation,
        }
    }

    fn assignment(runtime_generation: u64) -> core_v1::RuntimeAssignment {
        let runtime = identity("runtime-1", runtime_generation);
        core_v1::RuntimeAssignment {
            assignment_id: "assignment-1".to_string(),
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(runtime.clone()),
            }),
            operation: Some(identity("operation-1", 1)),
            attempt_id: "attempt-1".to_string(),
            lease: Some(semantic_v1::Lease {
                identity: Some(identity("lease-1", 1)),
                holder: Some(runtime.clone()),
                resources: vec![identity("resource-1", 1)],
                state: semantic_v1::LeaseState::Active as i32,
                fence_token: 7,
                expires_at: Some(timestamp(20_000)),
            }),
            workload_identity: Some(core_v1::WorkloadIdentity {
                identity: Some(identity("workload-1", 1)),
                scope: Some(core_v1::AccountScope {
                    user_id: "user-1".to_string(),
                    organization_id: "organization-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                runtime: Some(core_v1::RuntimeRef {
                    identity: Some(runtime),
                }),
                allowed_actions: vec!["operation.report".to_string()],
                expires_at: Some(timestamp(30_000)),
            }),
            profile: Some(core_v1::RuntimeProfile {
                image_digest: format!("sha256:{}", "1".repeat(64)),
                resolved_digest: format!("sha256:{}", "2".repeat(64)),
            }),
            desired_state: core_v1::DesiredRuntimeState::Running as i32,
            artifacts: vec![],
            local_artifacts: vec![],
        }
    }

    fn local_artifact(seed: char, size_bytes: u64) -> core_v1::ArtifactLocalInput {
        let digest_hex = seed.to_string().repeat(64);
        core_v1::ArtifactLocalInput {
            artifact_uri: format!("artifact://sha256/{digest_hex}"),
            digest: format!("sha256:{digest_hex}"),
            size_bytes,
            artifact_kind: "generic".to_string(),
            manifest_digest: String::new(),
        }
    }

    fn hello(
        attachment: core_v1::ExecutionAttachmentType,
        persistent: bool,
        restart: core_v1::RestartCapability,
    ) -> core_v1::ExecutionAgentHello {
        core_v1::ExecutionAgentHello {
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(identity("runtime-1", 1)),
            }),
            scope: Some(core_v1::AccountScope {
                user_id: String::new(),
                organization_id: "organization-1".to_string(),
                workspace_id: "workspace-1".to_string(),
            }),
            attachment_type: attachment as i32,
            persistence_class: if persistent {
                core_v1::PersistenceClass::Persistent as i32
            } else {
                core_v1::PersistenceClass::Ephemeral as i32
            },
            agent_version: "test".to_string(),
            min_protocol_version: 2,
            max_protocol_version: 2,
            resume_token: "opaque-resume".to_string(),
            capabilities: vec![crate::execution_capability(attachment, persistent, restart)],
            enrollment_proof: String::new(),
            node: Some(core_v1::ExecutionNodeDescriptor {
                node: Some(core_v1::NodeRef {
                    node_id: "node-1".to_string(),
                    node_epoch: 1,
                }),
                node_type: "fixture".to_string(),
                persistent: Some(persistent),
            }),
            restart_capability: restart as i32,
        }
    }

    #[test]
    fn cursor_accepts_identical_replay_and_rejects_gaps() {
        let mut cursor = ObservationCursor::default();
        assert_eq!(
            cursor.admit("frame-1", 1),
            Ok(AdmissionDisposition::Accepted)
        );
        assert_eq!(
            cursor.admit("frame-1", 1),
            Ok(AdmissionDisposition::Duplicate)
        );
        assert_eq!(
            cursor.admit("frame-3", 3).unwrap_err().reason_code,
            "SEQUENCE_GAP"
        );
    }

    #[test]
    fn resume_token_is_a_valid_reconnect_proof() {
        let hello = hello(
            core_v1::ExecutionAttachmentType::ContainerAgent,
            false,
            core_v1::RestartCapability::None,
        );
        assert_eq!(validate_hello(&hello), Ok(()));

        let mut incompatible = hello;
        incompatible.min_protocol_version = 3;
        incompatible.max_protocol_version = 3;
        assert_eq!(
            validate_hello(&incompatible).unwrap_err().reason_code,
            "PROTOCOL_INCOMPATIBLE"
        );
    }

    #[test]
    fn bootstrap_and_resume_credentials_are_mutually_exclusive() {
        let mut value = hello(
            core_v1::ExecutionAttachmentType::ContainerAgent,
            false,
            core_v1::RestartCapability::None,
        );
        value.enrollment_proof = "one-shot-proof".to_string();

        assert_eq!(
            validate_hello(&value).unwrap_err().reason_code,
            "CREDENTIAL_AMBIGUOUS"
        );
    }

    #[test]
    fn host_and_provider_managed_restart_contracts_are_admitted() {
        assert_eq!(
            validate_hello(&hello(
                core_v1::ExecutionAttachmentType::HostAgent,
                true,
                core_v1::RestartCapability::HostSupervised,
            )),
            Ok(())
        );
        assert_eq!(
            validate_hello(&hello(
                core_v1::ExecutionAttachmentType::ProviderManaged,
                false,
                core_v1::RestartCapability::ProviderSupervised,
            )),
            Ok(())
        );
    }

    #[test]
    fn stale_runtime_generation_cannot_admit_assignment() {
        let expected = semantic::Identity {
            id: "runtime-1".to_string(),
            generation: 2,
        };
        let error = validate_assignment(&expected, &assignment(1), 10_000).unwrap_err();
        assert_eq!(error.reason_code, "STALE_GENERATION");
    }

    #[test]
    fn current_generation_and_active_canonical_lease_are_admitted() {
        let expected = semantic::Identity {
            id: "runtime-1".to_string(),
            generation: 2,
        };
        let lease = validate_assignment(&expected, &assignment(2), 10_000).unwrap();
        assert_eq!(lease.holder, expected);
        assert_eq!(lease.fence_token, 7);
    }

    #[test]
    fn zero_byte_local_artifact_identity_is_admitted() {
        let expected = semantic::Identity {
            id: "runtime-1".to_string(),
            generation: 1,
        };
        let mut value = assignment(1);
        value.local_artifacts.push(local_artifact('a', 0));

        assert!(validate_assignment(&expected, &value, 10_000).is_ok());
    }

    #[test]
    fn local_artifact_rejects_noncanonical_kind_and_identity() {
        let expected = semantic::Identity {
            id: "runtime-1".to_string(),
            generation: 1,
        };
        let mut bad_kind = assignment(1);
        let mut input = local_artifact('b', 1);
        input.artifact_kind = "bad\nkind".to_string();
        bad_kind.local_artifacts.push(input);
        assert_eq!(
            validate_assignment(&expected, &bad_kind, 10_000)
                .unwrap_err()
                .reason_code,
            "ARTIFACT_KIND_INVALID"
        );

        let mut bad_uri = assignment(1);
        let mut input = local_artifact('c', 1);
        input.artifact_uri = format!("artifact://sha256/{}", "d".repeat(64));
        bad_uri.local_artifacts.push(input);
        assert_eq!(
            validate_assignment(&expected, &bad_uri, 10_000)
                .unwrap_err()
                .reason_code,
            "ARTIFACT_LOCAL_IDENTITY_INVALID"
        );
    }

    #[test]
    fn local_and_transfer_projections_cannot_repeat_a_digest() {
        let expected = semantic::Identity {
            id: "runtime-1".to_string(),
            generation: 1,
        };
        let mut value = assignment(1);
        let local = local_artifact('a', 1);
        value.artifacts.push(core_v1::ArtifactTransferSpec {
            artifact_uri: local.artifact_uri.clone(),
            digest: local.digest.clone(),
            size_bytes: 1,
            manifest_digest: String::new(),
            part_size_bytes: 1,
            part_digests: vec![format!("sha256:{}", "b".repeat(64))],
            sources: vec![core_v1::ArtifactTransferSource {
                peer_id: "peer-1".to_string(),
                replica_id: "replica-1".to_string(),
                locator: "https://source.example/artifact".to_string(),
                transfer_ticket: "opaque-ticket".to_string(),
            }],
            part_sources: vec![core_v1::ArtifactPartSource {
                part_index: 0,
                peer_id: "peer-1".to_string(),
                replica_id: "replica-1".to_string(),
            }],
            destination_peer_id: "peer-local".to_string(),
            artifact_kind: "generic".to_string(),
            ..Default::default()
        });
        value.local_artifacts.push(local);

        assert_eq!(
            validate_assignment(&expected, &value, 10_000)
                .unwrap_err()
                .reason_code,
            "ARTIFACT_ASSIGNMENT_DUPLICATE"
        );
    }

    #[test]
    fn legacy_replica_uri_cannot_bypass_artifact_plane_authorization() {
        let mut value = assignment(1);
        let artifact = core_v1::ArtifactTransferSpec {
            artifact_uri: format!("artifact://sha256/{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 1,
            manifest_digest: String::new(),
            part_size_bytes: 1,
            part_digests: vec![format!("sha256:{}", "b".repeat(64))],
            sources: Vec::new(),
            part_sources: Vec::new(),
            destination_peer_id: String::new(),
            artifact_kind: "generic".to_string(),
            ..Default::default()
        };
        let legacy_replica = b"https://legacy.example/artifact";
        let mut encoded = artifact.encode_to_vec();
        assert!(legacy_replica.len() < 128);
        encoded.extend([42, legacy_replica.len() as u8]);
        encoded.extend(legacy_replica);
        value.artifacts.push(
            core_v1::ArtifactTransferSpec::decode(encoded.as_slice())
                .expect("legacy wire field should remain decodable"),
        );

        assert_eq!(
            validate_assignment_payload(
                &semantic::Identity {
                    id: "runtime-1".to_string(),
                    generation: 1,
                },
                &value,
                10_000,
            )
            .unwrap_err()
            .reason_code,
            "ARTIFACT_TRANSFER_INVALID"
        );
    }
}
