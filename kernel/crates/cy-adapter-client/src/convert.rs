// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-adapter-client/src/convert.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::time::{SystemTime, UNIX_EPOCH};

use cy_kernel_api::{
    semantic::{self, Resource, ResourceState},
    EnforcementMode, EnforcementReport, ProviderError,
};
use cy_proto::{core_v1, hardware_v1, semantic_v1};

pub fn resource_from_proto(resource: semantic_v1::Resource) -> Result<Resource, ProviderError> {
    let identity = resource.identity.ok_or_else(|| {
        ProviderError::new(
            "hardware-adapter-protocol",
            "RESOURCE_IDENTITY_MISSING",
            "resource identity is required",
        )
    })?;
    let provider = resource.provider.ok_or_else(|| {
        ProviderError::new(
            "hardware-adapter-protocol",
            "RESOURCE_PROVIDER_MISSING",
            "resource provider identity is required",
        )
    })?;
    let resource = Resource {
        identity: identity_from_proto(identity),
        provider: identity_from_proto(provider),
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
            .map(|(key, quantity)| {
                (
                    key,
                    semantic::Quantity {
                        value: quantity.value,
                        unit: quantity.unit,
                    },
                )
            })
            .collect(),
        attributes: resource.attributes.into_iter().collect(),
        state: resource_state_from_proto(resource.state)?,
        reason_code: resource.reason_code,
        summary: resource.summary,
        links: resource
            .links
            .into_iter()
            .map(|link| {
                let peer = link.peer.ok_or_else(|| {
                    ProviderError::new(
                        "hardware-adapter-protocol",
                        "TOPOLOGY_PEER_MISSING",
                        "topology link peer identity is required",
                    )
                })?;
                Ok(semantic::TopologyLink {
                    peer: identity_from_proto(peer),
                    kind: link.kind,
                    properties: link.properties.into_iter().collect(),
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?,
    };
    resource.validate().map_err(|error| {
        ProviderError::new(
            "hardware-adapter-protocol",
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(resource)
}

pub(crate) fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

pub(crate) fn identity_from_proto(identity: semantic_v1::Identity) -> semantic::Identity {
    semantic::Identity {
        id: identity.id,
        generation: identity.generation,
    }
}

pub(crate) fn resource_state_from_proto(value: i32) -> Result<ResourceState, ProviderError> {
    match semantic_v1::ResourceState::try_from(value) {
        Ok(semantic_v1::ResourceState::Ready) => Ok(ResourceState::Ready),
        Ok(semantic_v1::ResourceState::Degraded) => Ok(ResourceState::Degraded),
        Ok(semantic_v1::ResourceState::Unavailable) => Ok(ResourceState::Unavailable),
        Ok(semantic_v1::ResourceState::Unspecified) | Err(_) => Err(ProviderError::new(
            "hardware-adapter-protocol",
            "UNKNOWN_ENUM_VALUE",
            "resource state must be a known, specified semantic value",
        )),
    }
}

pub(crate) fn enforcement_from_proto(report: core_v1::EnforcementReport) -> EnforcementReport {
    EnforcementReport {
        resource_kind: format!(
            "{:?}",
            core_v1::ResourceKind::try_from(report.resource_kind)
                .unwrap_or(core_v1::ResourceKind::Unspecified)
        )
        .to_ascii_lowercase(),
        mode: enforcement_mode_from_proto(report.mode),
        adapter_id: report.adapter_id,
        reason_code: report.reason_code,
    }
}

pub(crate) fn enforcement_mode_from_proto(value: i32) -> EnforcementMode {
    match core_v1::EnforcementMode::try_from(value).unwrap_or(core_v1::EnforcementMode::Unspecified)
    {
        core_v1::EnforcementMode::Hard => EnforcementMode::Hard,
        core_v1::EnforcementMode::Soft => EnforcementMode::Soft,
        core_v1::EnforcementMode::VisibilityOnly => EnforcementMode::VisibilityOnly,
        core_v1::EnforcementMode::ObserveOnly => EnforcementMode::ObserveOnly,
        _ => EnforcementMode::Unenforced,
    }
}

pub(crate) fn ensure_inventory_fresh(
    adapter_id: &str,
    inventory: &hardware_v1::HardwareInventory,
) -> Result<(), ProviderError> {
    let Some(sampled_at) = inventory.sampled_at.as_ref() else {
        return Err(ProviderError::new(
            adapter_id,
            "ADAPTER_FACT_TIMESTAMP_MISSING",
            "hardware inventory did not include sampled_at",
        ));
    };
    let Some(expires_at) = inventory.expires_at.as_ref() else {
        return Err(ProviderError::new(
            adapter_id,
            "ADAPTER_FACT_TIMESTAMP_MISSING",
            "hardware inventory did not include expires_at",
        ));
    };
    let sampled = timestamp_nanos(sampled_at).ok_or_else(|| {
        ProviderError::new(
            adapter_id,
            "ADAPTER_FACT_TIMESTAMP_INVALID",
            "sampled_at is outside the protobuf timestamp range",
        )
    })?;
    let expires = timestamp_nanos(expires_at).ok_or_else(|| {
        ProviderError::new(
            adapter_id,
            "ADAPTER_FACT_TIMESTAMP_INVALID",
            "expires_at is outside the protobuf timestamp range",
        )
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    if sampled >= expires || expires <= now {
        return Err(ProviderError::new(
            adapter_id,
            "ADAPTER_FACT_EXPIRED",
            "hardware inventory fact is expired or has an invalid TTL",
        ));
    }
    Ok(())
}

fn timestamp_nanos(timestamp: &prost_types::Timestamp) -> Option<u128> {
    if timestamp.seconds < 0 || !(0..1_000_000_000).contains(&timestamp.nanos) {
        return None;
    }
    u128::try_from(timestamp.seconds)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(timestamp.nanos as u128)
}
