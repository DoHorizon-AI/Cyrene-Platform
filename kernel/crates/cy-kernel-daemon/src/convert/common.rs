// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/common.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Shared timestamp, identity, context, and status conversions.
//!
//! 共享的 timestamp、identity、context 与 status 转换。
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_kernel_api::{semantic, AuthorityCallContext, NamespaceId, ProviderError};
use cy_proto::{core_v1, core_v2, semantic_v1};
use tonic::Status;

pub(crate) fn to_semantic_proto_identity(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

pub(crate) fn semantic_identity_from_proto(
    identity: Option<semantic_v1::Identity>,
    field: &str,
) -> Result<semantic::Identity, Status> {
    let identity =
        identity.ok_or_else(|| Status::invalid_argument(format!("{field} is required")))?;
    let identity = semantic::Identity {
        id: identity.id,
        generation: identity.generation,
    };
    identity.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(identity)
}

pub(crate) fn semantic_contract_revision_from_proto(
    revision: semantic_v1::ContractRevision,
) -> Option<semantic::ContractRevision> {
    let revision = semantic::ContractRevision {
        contract_id: revision.contract_id,
        major: revision.major,
        minor: revision.minor,
    };
    revision.validate().ok().map(|_| revision)
}

pub(crate) fn to_semantic_proto_contract_revision(
    revision: &semantic::ContractRevision,
) -> semantic_v1::ContractRevision {
    semantic_v1::ContractRevision {
        contract_id: revision.contract_id.clone(),
        major: revision.major,
        minor: revision.minor,
    }
}

pub(crate) fn validate_authority_context(
    context: Option<&core_v1::AuthorityCallContext>,
) -> Result<&core_v1::AuthorityCallContext, Status> {
    let context = context.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "AUTHORITY_CONTEXT_REQUIRED",
            "a negotiated authority call context is required",
        )
    })?;
    let offered = context.contract.clone().ok_or_else(|| {
        semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_NEGOTIATION_REQUIRED",
            "a selected semantic contract revision is required",
        )
    })?;
    let offered = semantic_contract_revision_from_proto(offered).ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "CONTRACT_REVISION_INVALID",
            "authority call contains an invalid semantic contract revision",
        )
    })?;
    let local = semantic::ContractRevision::current();
    if local.negotiate(&offered).as_ref() != Some(&offered) {
        return Err(semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_INCOMPATIBLE",
            "authority call did not use a revision selected by this Kernel",
        ));
    }
    if context.request_id.is_empty() && context.idempotency_key.is_empty() {
        return Err(semantic_status(
            tonic::Code::InvalidArgument,
            "REQUEST_ID_REQUIRED",
            "authority call requires request_id or idempotency_key",
        ));
    }
    Ok(context)
}

/// Projects a validated Core v1 envelope into the authority port metadata.
pub(crate) fn authority_call_context_from_proto(
    context: Option<&core_v1::AuthorityCallContext>,
) -> Result<AuthorityCallContext, Status> {
    let context = validate_authority_context(context)?;
    let contract = semantic_contract_revision_from_proto(
        context
            .contract
            .clone()
            .expect("validated authority context contains a contract"),
    )
    .expect("validated authority context contains a valid contract");
    Ok(AuthorityCallContext {
        contract,
        namespace: NamespaceId::default(),
        request_id: context.request_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
    })
}

/// Core v2 requires an explicit namespace. Core v1 intentionally keeps its
/// frozen envelope and is projected to `NamespaceId::default()` above.
pub(crate) fn authority_call_context_from_v2_proto(
    context: Option<&core_v2::AuthorityCallContext>,
) -> Result<AuthorityCallContext, Status> {
    let context = context.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "AUTHORITY_CONTEXT_REQUIRED",
            "a negotiated authority call context is required",
        )
    })?;
    if context.namespace.is_empty() {
        return Err(semantic_status(
            tonic::Code::InvalidArgument,
            "NAMESPACE_REQUIRED",
            "Core v2 authority calls require an explicit namespace",
        ));
    }
    let namespace = NamespaceId::new(context.namespace.clone()).map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            &error.reason_code,
            &error.message,
        )
    })?;
    let offered = context.contract.clone().ok_or_else(|| {
        semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_NEGOTIATION_REQUIRED",
            "a selected semantic contract revision is required",
        )
    })?;
    let contract = semantic_contract_revision_from_proto(offered).ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "CONTRACT_REVISION_INVALID",
            "authority call contains an invalid semantic contract revision",
        )
    })?;
    let local = semantic::ContractRevision::current();
    if local.negotiate(&contract).as_ref() != Some(&contract) {
        return Err(semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_INCOMPATIBLE",
            "authority call did not use a revision selected by this Kernel",
        ));
    }
    if context.request_id.is_empty() && context.idempotency_key.is_empty() {
        return Err(semantic_status(
            tonic::Code::InvalidArgument,
            "REQUEST_ID_REQUIRED",
            "authority call requires request_id or idempotency_key",
        ));
    }
    Ok(AuthorityCallContext {
        contract,
        namespace,
        request_id: context.request_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
    })
}

#[cfg(test)]
pub(crate) fn authority_lease_name(context: &core_v1::AuthorityCallContext) -> String {
    let key = if context.idempotency_key.is_empty() {
        &context.request_id
    } else {
        &context.idempotency_key
    };
    format!("lease-{key}")
}

pub(crate) fn provider_status(error: ProviderError) -> Status {
    let code = match error.reason_code.as_str() {
        "RESOURCE_NOT_FOUND" => tonic::Code::NotFound,
        "ADAPTER_REGISTRATION_INVALID"
        | "ADAPTER_ENDPOINT_INVALID"
        | "ADAPTER_REGISTRATION_EMPTY"
        | "ADAPTER_RESOURCE_ID_COLLISION"
        | "ADAPTER_BINDING_PROVENANCE_MISMATCH"
        | "RESOURCE_PROVENANCE_MISSING" => tonic::Code::InvalidArgument,
        _ => tonic::Code::Unavailable,
    };
    semantic_status(code, &error.reason_code, &error.message)
}

pub(crate) fn semantic_status(code: tonic::Code, reason_code: &str, message: &str) -> Status {
    let mut status = Status::new(code, message);
    if let Ok(value) = reason_code.parse() {
        status.metadata_mut().insert("x-cyrene-reason-code", value);
    }
    status
}

pub(crate) fn proto_duration(duration: prost_types::Duration) -> Result<Duration, Status> {
    if duration.seconds < 0 || duration.nanos < 0 {
        return Err(Status::invalid_argument(
            "duration seconds and nanos must be non-negative",
        ));
    }
    if duration.seconds > 315_576_000_000 || duration.nanos >= 1_000_000_000 {
        return Err(Status::invalid_argument(
            "duration exceeds the protobuf range",
        ));
    }
    Ok(Duration::new(
        duration.seconds as u64,
        duration.nanos as u32,
    ))
}

pub(crate) fn unix_ms_from_timestamp(
    timestamp: prost_types::Timestamp,
    field: &str,
) -> Result<u64, Status> {
    if timestamp.seconds < 0 || timestamp.nanos < 0 {
        return Err(Status::invalid_argument(format!(
            "{field} timestamp must be non-negative"
        )));
    }
    let millis = (timestamp.seconds as u64)
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add((timestamp.nanos as u64) / 1_000_000))
        .ok_or_else(|| Status::invalid_argument(format!("{field} timestamp overflowed u64 ms")))?;
    Ok(millis)
}

pub(crate) fn expires_after(duration: Duration) -> u64 {
    now_unix_ms().saturating_add(duration.as_millis().min(u64::MAX as u128) as u64)
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) fn timestamp_from_unix_ms(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: (unix_ms / 1000) as i64,
        nanos: ((unix_ms % 1000) * 1_000_000) as i32,
    }
}

pub(crate) fn to_proto_duration(duration: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: duration.as_secs() as i64,
        nanos: duration.subsec_nanos() as i32,
    }
}

pub(crate) fn now_timestamp() -> prost_types::Timestamp {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    prost_types::Timestamp {
        seconds: now.as_secs() as i64,
        nanos: now.subsec_nanos() as i32,
    }
}
