//! NodeControl protocol validation and semantic wire projections.
//!
//! The service implementation owns sessions and correlation state; this module
//! owns the pure frame validation, conversion, and status mapping helpers.

use super::*;

pub(super) fn validate_frame_identity(
    identity: &SessionIdentity,
    frame: &NodeToControlPlane,
) -> Result<(), DispatchError> {
    match identity {
        SessionIdentity::Host(node) => match frame.body.as_ref() {
            Some(node_to_control_plane::Body::Heartbeat(heartbeat)) => {
                let received = heartbeat.node.as_ref().ok_or_else(|| {
                    DispatchError::input(
                        "NODE_IDENTITY_REQUIRED",
                        "Host heartbeat requires NodeRef",
                    )
                })?;
                if NodeKey::from(received) != *node {
                    return Err(DispatchError::input(
                        "AUTHENTICATED_NODE_MISMATCH",
                        "Host heartbeat changed the authenticated NodeRef",
                    ));
                }
            }
            Some(node_to_control_plane::Body::CommandResult(result))
                if !result.command_id.is_empty() => {}
            Some(node_to_control_plane::Body::OperationEvent(_)) => {}
            _ => {
                return Err(DispatchError::input(
                    "HOST_FRAME_INVALID",
                    "Host session sent a frame outside the Host Agent protocol",
                ))
            }
        },
        SessionIdentity::Runtime(runtime, node) => {
            if let Some(node_to_control_plane::Body::Heartbeat(heartbeat)) = frame.body.as_ref() {
                let received = heartbeat.node.as_ref().ok_or_else(|| {
                    DispatchError::input(
                        "NODE_IDENTITY_REQUIRED",
                        "Runtime heartbeat requires NodeRef",
                    )
                })?;
                if NodeKey::from(received) != *node {
                    return Err(DispatchError::input(
                        "AUTHENTICATED_NODE_MISMATCH",
                        "Runtime heartbeat changed the authenticated NodeRef",
                    ));
                }
                return Ok(());
            }
            let received = runtime_for_frame(frame).ok_or_else(|| {
                DispatchError::input(
                    "RUNTIME_FRAME_INVALID",
                    "Runtime session sent a frame without its Runtime identity",
                )
            })?;
            if &received != runtime {
                return Err(DispatchError::input(
                    "AUTHENTICATED_RUNTIME_MISMATCH",
                    "Runtime frame changed the authenticated Runtime generation",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_user_and_actions(
    user_id: &str,
    allowed_actions: &[String],
) -> Result<(), DispatchError> {
    if user_id.trim().is_empty() {
        return Err(DispatchError::input(
            "WORKLOAD_USER_REQUIRED",
            "workload identity requires a non-empty user id",
        ));
    }
    if user_id.len() > MAX_WORKLOAD_USER_BYTES {
        return Err(DispatchError::input(
            "WORKLOAD_USER_LIMIT",
            format!("workload identity user id allows at most {MAX_WORKLOAD_USER_BYTES} bytes"),
        ));
    }
    if allowed_actions.is_empty() {
        return Err(DispatchError::input(
            "WORKLOAD_ACTIONS_INVALID",
            "workload identity requires at least one action",
        ));
    }
    if allowed_actions.len() > MAX_WORKLOAD_ACTIONS {
        return Err(DispatchError::input(
            "WORKLOAD_ACTIONS_LIMIT",
            format!("workload identity allows at most {MAX_WORKLOAD_ACTIONS} actions"),
        ));
    }
    let mut unique = std::collections::BTreeSet::new();
    for action in allowed_actions {
        if action.trim().is_empty() {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_INVALID",
                "workload identity actions cannot be empty",
            ));
        }
        if action.len() > MAX_WORKLOAD_ACTION_BYTES {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_LIMIT",
                format!(
                    "workload identity actions allow at most {MAX_WORKLOAD_ACTION_BYTES} bytes"
                ),
            ));
        }
        if !unique.insert(action) {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_DUPLICATE",
                "workload identity actions must be unique",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_workload_identity_payload(
    workload: Option<&core_v1::WorkloadIdentity>,
) -> Result<(), DispatchError> {
    let workload = workload.ok_or_else(|| {
        DispatchError::input(
            "WORKLOAD_IDENTITY_REQUIRED",
            "Runtime assignment requires a workload identity",
        )
    })?;
    let scope = workload.scope.as_ref().ok_or_else(|| {
        DispatchError::input(
            "WORKLOAD_SCOPE_REQUIRED",
            "Runtime assignment workload identity requires an account scope",
        )
    })?;
    validate_user_and_actions(&scope.user_id, &workload.allowed_actions)?;
    Ok(())
}

pub(super) fn semantic_identity_from_proto(
    identity: &semantic_v1::Identity,
) -> Result<semantic::Identity, DispatchError> {
    let identity = semantic::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    };
    identity.validate().map_err(|error| {
        DispatchError::input(
            error.reason_code,
            format!("invalid semantic identity: {}", error.message),
        )
    })?;
    Ok(identity)
}

pub(super) fn runtime_for_frame(frame: &NodeToControlPlane) -> Option<semantic::Identity> {
    let runtime = match frame.body.as_ref()? {
        node_to_control_plane::Body::AssignmentAck(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::ExecutionInventory(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeHeartbeat(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::LeaseRenewal(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeObservation(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeProgress(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::StructuredEvent(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::LogReference(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::StopAck(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::Heartbeat(_) => return None,
        _ => return None,
    }?;
    runtime
        .identity
        .as_ref()
        .map(|identity| semantic::Identity {
            id: identity.id.clone(),
            generation: identity.generation,
        })
}

pub(super) fn runtime_identity(
    hello: &ExecutionAgentHello,
) -> Result<semantic::Identity, DispatchError> {
    let identity = hello
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.identity.as_ref())
        .ok_or_else(|| {
            DispatchError::input("RUNTIME_IDENTITY_REQUIRED", "Runtime identity is required")
        })?;
    Ok(semantic::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    })
}

pub(super) fn authority_lease_result(
    result: KernelCommandResult,
) -> Result<semantic_v1::Lease, DispatchError> {
    match result.outcome {
        Some(kernel_command_result::Outcome::Authority(authority)) => match authority.outcome {
            Some(kernel_authority_command_result::Outcome::Lease(lease)) => Ok(lease),
            _ => Err(DispatchError::unknown(
                "Kernel authority command returned an unexpected typed outcome",
            )),
        },
        Some(kernel_command_result::Outcome::Error(status)) => Err(kernel_status_error(status)),
        _ => Err(DispatchError::unknown(
            "Node Agent returned an unexpected Kernel command outcome",
        )),
    }
}

pub(super) fn kernel_status_error(status: cy_proto::google::rpc::Status) -> DispatchError {
    let structured = status.details.iter().find_map(|detail| {
        (detail.type_url == "type.googleapis.com/cyrene.semantic.v1.Rejection")
            .then(|| semantic_v1::Rejection::decode(detail.value.as_slice()).ok())
            .flatten()
    });
    if let Some(rejection) = structured {
        return DispatchError::authority(semantic::Rejection::new(
            rejection.reason_code,
            rejection.message,
        ));
    }
    let code = Code::from_i32(status.code);
    if matches!(
        code,
        Code::Ok
            | Code::Cancelled
            | Code::Unknown
            | Code::DeadlineExceeded
            | Code::Aborted
            | Code::Internal
            | Code::Unavailable
            | Code::DataLoss
    ) {
        return DispatchError::unknown(format!(
            "Kernel authority transport returned {code}: {}",
            status.message
        ));
    }
    DispatchError::authority(semantic::Rejection::new(
        "KERNEL_AUTHORITY_REJECTED",
        status.message,
    ))
}

pub(super) fn context_to_proto(context: &AuthorityCallContext) -> core_v1::AuthorityCallContext {
    core_v1::AuthorityCallContext {
        contract: Some(semantic_v1::ContractRevision {
            contract_id: context.contract.contract_id.clone(),
            major: context.contract.major,
            minor: context.contract.minor,
        }),
        request_id: context.request_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
    }
}

pub(super) fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

pub(super) fn resource_query_to_proto(
    query: &semantic::ResourceQuery,
) -> semantic_v1::ResourceQuery {
    semantic_v1::ResourceQuery {
        resource_class: query.resource_class.clone(),
        count: query.count,
        required_capabilities: query
            .required_capabilities
            .iter()
            .map(|requirement| semantic_v1::CapabilityRequirement {
                id: requirement.id.clone(),
                minimum_revision: requirement.minimum_revision,
                required_properties: requirement
                    .required_properties
                    .clone()
                    .into_iter()
                    .collect(),
            })
            .collect(),
        minimum_capacity: query
            .minimum_capacity
            .iter()
            .map(|(key, quantity)| {
                (
                    key.clone(),
                    semantic_v1::Quantity {
                        value: quantity.value,
                        unit: quantity.unit.clone(),
                    },
                )
            })
            .collect(),
    }
}

pub(super) fn duration_to_proto(
    duration: Duration,
) -> Result<prost_types::Duration, DispatchError> {
    let seconds = i64::try_from(duration.as_secs()).map_err(|_| {
        DispatchError::input("DURATION_INVALID", "duration seconds exceed Protobuf range")
    })?;
    let nanos = i32::try_from(duration.subsec_nanos()).map_err(|_| {
        DispatchError::input("DURATION_INVALID", "duration nanos exceed Protobuf range")
    })?;
    Ok(prost_types::Duration { seconds, nanos })
}

pub(super) fn contract_to_proto() -> semantic_v1::ContractRevision {
    let revision = semantic::ContractRevision::current();
    semantic_v1::ContractRevision {
        contract_id: revision.contract_id,
        major: revision.major,
        minor: revision.minor,
    }
}

pub(super) fn supported_contract(revision: &semantic_v1::ContractRevision) -> bool {
    revision == &contract_to_proto()
}

pub(super) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(super) fn timestamp_unix_ms(timestamp: &prost_types::Timestamp) -> Option<u64> {
    if timestamp.seconds <= 0 || timestamp.nanos < 0 || timestamp.nanos % 1_000_000 != 0 {
        return None;
    }
    u64::try_from(timestamp.seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(u64::try_from(timestamp.nanos / 1_000_000).ok()?)
}

pub(super) fn now_timestamp() -> prost_types::Timestamp {
    timestamp_from_unix_ms(now_unix_ms())
}

pub(super) fn timestamp_from_unix_ms(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(unix_ms / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((unix_ms % 1000) * 1_000_000).unwrap_or_default(),
    }
}

pub(super) fn status_from_dispatch(error: DispatchError) -> Status {
    let message = format!("{}: {}", error.reason_code, error.message);
    if error.reconciliation_required {
        Status::aborted(message)
    } else if error.reason_code.contains("AUTHENTICATED")
        || error.reason_code.contains("CERTIFICATE")
        || error.reason_code.contains("RESUME_TOKEN")
    {
        Status::unauthenticated(message)
    } else {
        Status::failed_precondition(message)
    }
}
