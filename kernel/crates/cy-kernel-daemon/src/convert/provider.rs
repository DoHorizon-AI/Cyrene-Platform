// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/provider.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use cy_kernel_api::{semantic, AuthorityCallContext, NamespaceId};
use cy_proto::{provider_v1, semantic_v1};
use tonic::Status;

use super::{
    semantic_endpoint_from_proto, semantic_identity_from_proto, semantic_status,
    timestamp_from_unix_ms, to_semantic_proto_endpoint, to_semantic_proto_identity,
    to_semantic_proto_resource, to_semantic_proto_worker, unix_ms_from_timestamp,
};

pub(crate) fn provider_call_context_from_proto(
    context: Option<&provider_v1::ProviderCallContext>,
) -> Result<AuthorityCallContext, Status> {
    let context = context.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "AUTHORITY_CONTEXT_REQUIRED",
            "a negotiated provider call context is required",
        )
    })?;
    let namespace = NamespaceId::new(context.namespace.clone()).map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            &error.reason_code,
            &error.message,
        )
    })?;
    let contract = context.contract.clone().ok_or_else(|| {
        semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_NEGOTIATION_REQUIRED",
            "a selected semantic contract revision is required",
        )
    })?;
    let contract = semantic::ContractRevision {
        contract_id: contract.contract_id,
        major: contract.major,
        minor: contract.minor,
    };
    if semantic::ContractRevision::current()
        .negotiate(&contract)
        .as_ref()
        != Some(&contract)
    {
        return Err(semantic_status(
            tonic::Code::FailedPrecondition,
            "CONTRACT_INCOMPATIBLE",
            "provider call did not use a compatible semantic contract revision",
        ));
    }
    if context.request_id.is_empty() && context.idempotency_key.is_empty() {
        return Err(semantic_status(
            tonic::Code::InvalidArgument,
            "REQUEST_ID_REQUIRED",
            "provider call requires request_id or idempotency_key",
        ));
    }
    Ok(AuthorityCallContext {
        contract,
        namespace,
        request_id: context.request_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
    })
}

pub(crate) fn semantic_provider_from_proto(
    provider: semantic_v1::Provider,
) -> Result<semantic::Provider, Status> {
    let state = match semantic_v1::ProviderState::try_from(provider.state) {
        Ok(semantic_v1::ProviderState::Ready) => semantic::ProviderState::Ready,
        Ok(semantic_v1::ProviderState::Degraded) => semantic::ProviderState::Degraded,
        Ok(semantic_v1::ProviderState::Unavailable) => semantic::ProviderState::Unavailable,
        _ => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "provider state cannot be unspecified or unknown",
            ));
        }
    };
    let provider = semantic::Provider {
        identity: semantic_identity_from_proto(provider.identity, "provider identity")?,
        state,
        capabilities: provider
            .capabilities
            .into_iter()
            .map(|capability| semantic::Capability {
                id: capability.id,
                revision: capability.revision,
                properties: capability.properties.into_iter().collect(),
            })
            .collect(),
    };
    provider.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(provider)
}

pub(crate) fn to_semantic_proto_provider(provider: &semantic::Provider) -> semantic_v1::Provider {
    semantic_v1::Provider {
        identity: Some(to_semantic_proto_identity(&provider.identity)),
        state: match provider.state {
            semantic::ProviderState::Ready => semantic_v1::ProviderState::Ready,
            semantic::ProviderState::Degraded => semantic_v1::ProviderState::Degraded,
            semantic::ProviderState::Unavailable => semantic_v1::ProviderState::Unavailable,
        } as i32,
        capabilities: provider
            .capabilities
            .iter()
            .map(|capability| semantic_v1::Capability {
                id: capability.id.clone(),
                revision: capability.revision,
                properties: capability.properties.clone().into_iter().collect(),
            })
            .collect(),
    }
}

pub(crate) fn semantic_provider_snapshot_from_proto(
    snapshot: semantic_v1::ProviderSnapshot,
) -> Result<semantic::ProviderSnapshot, Status> {
    let snapshot = semantic::ProviderSnapshot {
        provider: semantic_identity_from_proto(snapshot.provider, "snapshot provider")?,
        snapshot_generation: snapshot.snapshot_generation,
        resources: snapshot
            .resources
            .into_iter()
            .map(semantic_resource_from_proto)
            .collect::<Result<Vec<_>, _>>()?,
        workers: snapshot
            .workers
            .into_iter()
            .map(semantic_observed_worker_from_proto)
            .collect::<Result<Vec<_>, _>>()?,
        endpoints: snapshot
            .endpoints
            .into_iter()
            .map(semantic_endpoint_from_proto)
            .collect::<Result<Vec<_>, _>>()?,
        sampled_at_unix_ms: unix_ms_from_timestamp(
            snapshot.sampled_at.ok_or_else(|| {
                Status::invalid_argument("provider snapshot sampled_at is required")
            })?,
            "provider snapshot sampled_at",
        )?,
        expires_at_unix_ms: unix_ms_from_timestamp(
            snapshot.expires_at.ok_or_else(|| {
                Status::invalid_argument("provider snapshot expires_at is required")
            })?,
            "provider snapshot expires_at",
        )?,
    };
    snapshot.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(snapshot)
}

pub(crate) fn to_semantic_proto_provider_snapshot(
    snapshot: &semantic::ProviderSnapshot,
) -> semantic_v1::ProviderSnapshot {
    semantic_v1::ProviderSnapshot {
        provider: Some(to_semantic_proto_identity(&snapshot.provider)),
        snapshot_generation: snapshot.snapshot_generation,
        resources: snapshot
            .resources
            .iter()
            .map(to_semantic_proto_resource)
            .collect(),
        workers: snapshot
            .workers
            .iter()
            .map(to_semantic_proto_worker)
            .collect(),
        endpoints: snapshot
            .endpoints
            .iter()
            .map(to_semantic_proto_endpoint)
            .collect(),
        sampled_at: Some(timestamp_from_unix_ms(snapshot.sampled_at_unix_ms)),
        expires_at: Some(timestamp_from_unix_ms(snapshot.expires_at_unix_ms)),
    }
}

fn semantic_resource_from_proto(
    resource: semantic_v1::Resource,
) -> Result<semantic::Resource, Status> {
    let state = match semantic_v1::ResourceState::try_from(resource.state) {
        Ok(semantic_v1::ResourceState::Ready) => semantic::ResourceState::Ready,
        Ok(semantic_v1::ResourceState::Degraded) => semantic::ResourceState::Degraded,
        Ok(semantic_v1::ResourceState::Unavailable) => semantic::ResourceState::Unavailable,
        _ => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "resource state cannot be unspecified or unknown",
            ));
        }
    };
    let resource = semantic::Resource {
        identity: semantic_identity_from_proto(resource.identity, "resource identity")?,
        provider: semantic_identity_from_proto(resource.provider, "resource provider")?,
        resource_class: resource.resource_class,
        capabilities: resource
            .capabilities
            .into_iter()
            .map(|capability| semantic::Capability {
                id: capability.id,
                revision: capability.revision,
                properties: capability.properties.into_iter().collect(),
            })
            .collect(),
        capacity: resource
            .capacity
            .into_iter()
            .map(|(id, quantity)| {
                (
                    id,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
        attributes: resource.attributes.into_iter().collect(),
        state,
        reason_code: resource.reason_code,
        summary: resource.summary,
        links: resource
            .links
            .into_iter()
            .map(|link| {
                Ok(semantic::TopologyLink {
                    peer: semantic_identity_from_proto(link.peer, "topology peer")?,
                    kind: link.kind,
                    properties: link.properties.into_iter().collect(),
                })
            })
            .collect::<Result<Vec<_>, Status>>()?,
    };
    resource.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(resource)
}

fn semantic_observed_worker_from_proto(
    worker: semantic_v1::Worker,
) -> Result<semantic::Worker, Status> {
    let state = match semantic_v1::WorkerState::try_from(worker.state) {
        Ok(semantic_v1::WorkerState::Registered) => semantic::WorkerState::Registered,
        Ok(semantic_v1::WorkerState::Starting) => semantic::WorkerState::Starting,
        Ok(semantic_v1::WorkerState::Running) => semantic::WorkerState::Running,
        Ok(semantic_v1::WorkerState::Draining) => semantic::WorkerState::Draining,
        Ok(semantic_v1::WorkerState::Stopped) => semantic::WorkerState::Stopped,
        Ok(semantic_v1::WorkerState::Failed) => semantic::WorkerState::Failed,
        Ok(semantic_v1::WorkerState::Lost) => semantic::WorkerState::Lost,
        _ => {
            return Err(semantic_status(
                tonic::Code::InvalidArgument,
                "UNKNOWN_ENUM_VALUE",
                "worker state cannot be unspecified or unknown",
            ));
        }
    };
    let worker = semantic::Worker {
        identity: semantic_identity_from_proto(worker.identity, "worker identity")?,
        principal: semantic_identity_from_proto(worker.principal, "worker principal")?,
        provider: semantic_identity_from_proto(worker.provider, "worker provider")?,
        lease: semantic_identity_from_proto(worker.lease, "worker lease")?,
        state,
        execution_ref: worker.execution_ref,
        limits: worker
            .limits
            .into_iter()
            .map(|(id, quantity)| {
                (
                    id,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
    };
    worker.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(worker)
}
