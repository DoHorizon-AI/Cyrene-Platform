// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/operation.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Conversions for operation, endpoint, and operation-status projections.
//!
//! Operation、Endpoint 与 Operation 状态 projection 的转换。
use cy_kernel_api::semantic;
use cy_proto::semantic_v1;
use tonic::Status;

use super::common::{
    semantic_identity_from_proto, semantic_status, timestamp_from_unix_ms,
    to_semantic_proto_identity, unix_ms_from_timestamp,
};

pub(crate) fn semantic_endpoint_from_proto(
    endpoint: semantic_v1::Endpoint,
) -> Result<semantic::Endpoint, Status> {
    let endpoint = semantic::Endpoint {
        identity: semantic_identity_from_proto(endpoint.identity, "endpoint identity")?,
        provider: semantic_identity_from_proto(endpoint.provider, "endpoint provider")?,
        owner: semantic_identity_from_proto(endpoint.owner, "endpoint owner")?,
        transport: endpoint.transport,
        schema_id: endpoint.schema_id,
        capabilities: endpoint
            .capabilities
            .into_iter()
            .map(|capability| semantic::Capability {
                id: capability.id,
                revision: capability.revision,
                properties: capability.properties.into_iter().collect(),
            })
            .collect(),
        public_attributes: endpoint.public_attributes.into_iter().collect(),
        connection_ref: endpoint.connection_ref,
        credential_ref: (!endpoint.credential_ref.is_empty()).then_some(endpoint.credential_ref),
    };
    endpoint.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(endpoint)
}

pub(crate) fn to_semantic_proto_endpoint(endpoint: &semantic::Endpoint) -> semantic_v1::Endpoint {
    semantic_v1::Endpoint {
        identity: Some(to_semantic_proto_identity(&endpoint.identity)),
        provider: Some(to_semantic_proto_identity(&endpoint.provider)),
        owner: Some(to_semantic_proto_identity(&endpoint.owner)),
        transport: endpoint.transport.clone(),
        schema_id: endpoint.schema_id.clone(),
        capabilities: endpoint
            .capabilities
            .iter()
            .map(|capability| semantic_v1::Capability {
                id: capability.id.clone(),
                revision: capability.revision,
                properties: capability.properties.clone().into_iter().collect(),
            })
            .collect(),
        public_attributes: endpoint.public_attributes.clone().into_iter().collect(),
        connection_ref: endpoint.connection_ref.clone(),
        credential_ref: endpoint.credential_ref.clone().unwrap_or_default(),
    }
}

pub(crate) fn semantic_endpoint_grant_from_proto(
    grant: semantic_v1::EndpointGrant,
) -> Result<semantic::EndpointGrant, Status> {
    let grant = semantic::EndpointGrant {
        identity: semantic_identity_from_proto(grant.identity, "endpoint grant identity")?,
        endpoint: semantic_identity_from_proto(grant.endpoint, "endpoint grant endpoint")?,
        grantee: semantic_identity_from_proto(grant.grantee, "endpoint grant grantee")?,
        lease: semantic_identity_from_proto(grant.lease, "endpoint grant lease")?,
        fence_token: grant.fence_token,
        expires_at_unix_ms: unix_ms_from_timestamp(
            grant
                .expires_at
                .ok_or_else(|| Status::invalid_argument("endpoint grant expiry is required"))?,
            "endpoint grant expiry",
        )?,
    };
    grant.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(grant)
}

pub(crate) fn to_semantic_proto_endpoint_grant(
    grant: &semantic::EndpointGrant,
) -> semantic_v1::EndpointGrant {
    semantic_v1::EndpointGrant {
        identity: Some(to_semantic_proto_identity(&grant.identity)),
        endpoint: Some(to_semantic_proto_identity(&grant.endpoint)),
        grantee: Some(to_semantic_proto_identity(&grant.grantee)),
        lease: Some(to_semantic_proto_identity(&grant.lease)),
        fence_token: grant.fence_token,
        expires_at: Some(timestamp_from_unix_ms(grant.expires_at_unix_ms)),
    }
}

pub(crate) fn semantic_operation_from_proto(
    operation: semantic_v1::Operation,
) -> Result<semantic::Operation, Status> {
    let state = semantic_v1::OperationState::try_from(operation.state).map_err(|_| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "UNKNOWN_ENUM_VALUE",
            "operation state is unknown",
        )
    })?;
    let state = match state {
        semantic_v1::OperationState::Created => semantic::OperationState::Created,
        semantic_v1::OperationState::Pending => semantic::OperationState::Pending,
        semantic_v1::OperationState::Running => semantic::OperationState::Running,
        semantic_v1::OperationState::Succeeded => semantic::OperationState::Succeeded,
        semantic_v1::OperationState::Failed => semantic::OperationState::Failed,
        semantic_v1::OperationState::Cancelling => semantic::OperationState::Cancelling,
        semantic_v1::OperationState::Cancelled => semantic::OperationState::Cancelled,
        semantic_v1::OperationState::Lost => semantic::OperationState::Lost,
        semantic_v1::OperationState::Unspecified => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "operation state cannot be UNSPECIFIED",
            ));
        }
    };
    let operation = semantic::Operation {
        identity: semantic_identity_from_proto(operation.identity, "operation identity")?,
        owner: semantic_identity_from_proto(operation.owner, "operation owner")?,
        executor: semantic_identity_from_proto(operation.executor, "operation executor")?,
        kind: operation.kind,
        state,
        deadline_unix_ms: operation
            .deadline
            .map(|timestamp| unix_ms_from_timestamp(timestamp, "operation deadline"))
            .transpose()?,
        parent: operation
            .parent
            .map(|identity| semantic_identity_from_proto(Some(identity), "operation parent"))
            .transpose()?,
        metadata: operation.metadata.into_iter().collect(),
    };
    operation.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(operation)
}

pub(crate) fn to_semantic_proto_operation(
    operation: &semantic::Operation,
) -> semantic_v1::Operation {
    semantic_v1::Operation {
        identity: Some(to_semantic_proto_identity(&operation.identity)),
        owner: Some(to_semantic_proto_identity(&operation.owner)),
        executor: Some(to_semantic_proto_identity(&operation.executor)),
        kind: operation.kind.clone(),
        state: match operation.state {
            semantic::OperationState::Created => semantic_v1::OperationState::Created,
            semantic::OperationState::Pending => semantic_v1::OperationState::Pending,
            semantic::OperationState::Running => semantic_v1::OperationState::Running,
            semantic::OperationState::Succeeded => semantic_v1::OperationState::Succeeded,
            semantic::OperationState::Failed => semantic_v1::OperationState::Failed,
            semantic::OperationState::Cancelling => semantic_v1::OperationState::Cancelling,
            semantic::OperationState::Cancelled => semantic_v1::OperationState::Cancelled,
            semantic::OperationState::Lost => semantic_v1::OperationState::Lost,
        } as i32,
        deadline: operation.deadline_unix_ms.map(timestamp_from_unix_ms),
        parent: operation.parent.as_ref().map(to_semantic_proto_identity),
        metadata: operation.metadata.clone().into_iter().collect(),
    }
}
