//! NVIDIA adapter host: the only CYRENE component allowed to interrogate this
//! vendor's command-line tools, sysfs layout, and device nodes.

pub mod discovery;
mod peer;

#[cfg(test)]
mod tests;

pub use peer::client_peer_credentials_allowed;
#[cfg(unix)]
pub use peer::verify_client_peer;

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic::{self, Resource},
    DeviceBinding, EnforcementMode, HostInventoryProvider, ProviderError, ResourceProvider,
};
use cy_proto::{core_v1, hardware_v1, semantic_v1};

pub const PROTOCOL_VERSION: u32 = 2;

/// Handle one bounded local-adapter request.  The caller owns transport and
/// process supervision; this function has no access to the Kernel process.
#[allow(deprecated)]
pub fn handle_request(
    provider: &discovery::NvidiaSmiProvider,
    request: hardware_v1::AdapterRequest,
) -> hardware_v1::AdapterResponse {
    let adapter_id = provider.adapter_id().to_string();
    if request.protocol_version != PROTOCOL_VERSION {
        return error_response(
            &adapter_id,
            "ADAPTER_PROTOCOL_VERSION",
            "unsupported adapter protocol version",
            false,
        );
    }

    match request.body {
        Some(hardware_v1::adapter_request::Body::GetInventory(_)) => {
            match HostInventoryProvider::probe_inventory(provider) {
                Ok(inventory) => hardware_v1::AdapterResponse {
                    protocol_version: PROTOCOL_VERSION,
                    adapter_id,
                    body: Some(hardware_v1::adapter_response::Body::Inventory(
                        hardware_v1::HardwareInventory {
                            generation: inventory.generation,
                            devices: Vec::new(),
                            facts: inventory
                                .capabilities
                                .facts
                                .into_iter()
                                .map(|fact| hardware_v1::AdapterFact {
                                    name: fact.name,
                                    available: fact.available,
                                    required: fact.required,
                                    detail: fact.detail,
                                })
                                .collect(),
                            enforcement: inventory
                                .capabilities
                                .enforcement
                                .into_iter()
                                .map(enforcement_to_proto)
                                .collect(),
                            sampled_at: Some(now_timestamp()),
                            expires_at: Some(timestamp_after(Duration::from_secs(15))),
                            resources: inventory.resources.iter().map(resource_to_proto).collect(),
                        },
                    )),
                },
                Err(error) => provider_error_response(&adapter_id, error),
            }
        }
        Some(hardware_v1::adapter_request::Body::CreateBinding(request)) => {
            match create_binding(provider, &request) {
                Ok((binding, resource)) => hardware_v1::AdapterResponse {
                    protocol_version: PROTOCOL_VERSION,
                    adapter_id,
                    body: Some(hardware_v1::adapter_response::Body::Binding(
                        binding_to_proto(binding, resource),
                    )),
                },
                Err(error) => provider_error_response(&adapter_id, error),
            }
        }
        None => error_response(
            &adapter_id,
            "ADAPTER_REQUEST_EMPTY",
            "request body is required",
            false,
        ),
    }
}

fn now_timestamp() -> prost_types::Timestamp {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    prost_types::Timestamp {
        seconds: elapsed.as_secs().min(i64::MAX as u64) as i64,
        nanos: elapsed.subsec_nanos() as i32,
    }
}

fn timestamp_after(duration: Duration) -> prost_types::Timestamp {
    let mut timestamp = now_timestamp();
    timestamp.seconds = timestamp
        .seconds
        .saturating_add(duration.as_secs().min(i64::MAX as u64) as i64);
    timestamp.nanos = timestamp
        .nanos
        .saturating_add(duration.subsec_nanos() as i32);
    if timestamp.nanos >= 1_000_000_000 {
        timestamp.seconds = timestamp.seconds.saturating_add(1);
        timestamp.nanos -= 1_000_000_000;
    }
    timestamp
}

#[allow(deprecated)]
fn create_binding(
    provider: &discovery::NvidiaSmiProvider,
    request: &hardware_v1::CreateBindingRequest,
) -> Result<(DeviceBinding, semantic::Identity), ProviderError> {
    let inventory = HostInventoryProvider::probe_inventory(provider)?;
    if request.expected_inventory_generation != 0
        && request.expected_inventory_generation != inventory.generation
    {
        return Err(ProviderError::new(
            provider.adapter_id(),
            "INVENTORY_GENERATION_STALE",
            "binding request is based on a stale hardware inventory",
        ));
    }
    let requested_resource = request.resource.as_ref();
    let resource_id = requested_resource
        .map(|resource| resource.id.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or(&request.device_id);
    let resource = inventory
        .resources
        .iter()
        .find(|resource| resource.identity.id == resource_id)
        .ok_or_else(|| {
            ProviderError::new(
                provider.adapter_id(),
                "RESOURCE_NOT_FOUND",
                "resource is absent",
            )
        })?;
    if requested_resource.is_some_and(|requested| {
        requested.generation != 0 && requested.generation != resource.identity.generation
    }) {
        return Err(ProviderError::new(
            provider.adapter_id(),
            "RESOURCE_GENERATION_STALE",
            "binding request is based on a stale resource generation",
        ));
    }
    provider
        .create_binding(resource)
        .map(|binding| (binding, resource.identity.clone()))
}

fn provider_error_response(adapter_id: &str, error: ProviderError) -> hardware_v1::AdapterResponse {
    error_response(adapter_id, &error.reason_code, &error.message, true)
}

fn error_response(
    adapter_id: &str,
    reason_code: &str,
    message: &str,
    retryable: bool,
) -> hardware_v1::AdapterResponse {
    hardware_v1::AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        adapter_id: adapter_id.to_string(),
        body: Some(hardware_v1::adapter_response::Body::Error(
            hardware_v1::AdapterError {
                reason_code: reason_code.to_string(),
                message: message.to_string(),
                retryable,
            },
        )),
    }
}

fn resource_to_proto(resource: &Resource) -> semantic_v1::Resource {
    semantic_v1::Resource {
        identity: Some(identity_to_proto(&resource.identity)),
        provider: Some(identity_to_proto(&resource.provider)),
        resource_class: resource.resource_class.clone(),
        capabilities: resource
            .capabilities
            .iter()
            .map(|capability| semantic_v1::Capability {
                id: capability.id.clone(),
                revision: capability.revision,
                properties: capability.properties.clone().into_iter().collect(),
            })
            .collect(),
        capacity: resource
            .capacity
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
        attributes: resource.attributes.clone().into_iter().collect(),
        state: resource_state_to_proto(resource.state),
        reason_code: resource.reason_code.clone(),
        summary: resource.summary.clone(),
        links: resource
            .links
            .iter()
            .map(|link| semantic_v1::TopologyLink {
                peer: Some(identity_to_proto(&link.peer)),
                kind: link.kind.clone(),
                properties: link.properties.clone().into_iter().collect(),
            })
            .collect(),
    }
}

fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn resource_state_to_proto(state: semantic::ResourceState) -> i32 {
    match state {
        semantic::ResourceState::Ready => semantic_v1::ResourceState::Ready as i32,
        semantic::ResourceState::Degraded => semantic_v1::ResourceState::Degraded as i32,
        semantic::ResourceState::Unavailable => semantic_v1::ResourceState::Unavailable as i32,
    }
}

#[allow(deprecated)]
fn binding_to_proto(
    binding: DeviceBinding,
    resource: semantic::Identity,
) -> hardware_v1::DeviceBinding {
    hardware_v1::DeviceBinding {
        device_id: binding.resource_id.clone(),
        nodes: binding
            .nodes
            .into_iter()
            .map(|node| hardware_v1::DeviceNode {
                path: node.path.to_string_lossy().into_owned(),
                major: node.major,
                minor: node.minor,
                required: node.required,
            })
            .collect(),
        environment: binding.environment.into_iter().collect::<HashMap<_, _>>(),
        required_gids: binding.required_gids,
        enforcement: enforcement_mode_to_proto(binding.enforcement),
        reason_code: binding.reason_code,
        resource: Some(identity_to_proto(&resource)),
    }
}

fn enforcement_to_proto(report: cy_kernel_api::EnforcementReport) -> core_v1::EnforcementReport {
    core_v1::EnforcementReport {
        resource_kind: core_v1::ResourceKind::Unspecified as i32,
        mode: enforcement_mode_to_proto(report.mode),
        adapter_id: report.adapter_id,
        reason_code: report.reason_code,
    }
}

fn enforcement_mode_to_proto(value: EnforcementMode) -> i32 {
    match value {
        EnforcementMode::Hard => core_v1::EnforcementMode::Hard as i32,
        EnforcementMode::Soft => core_v1::EnforcementMode::Soft as i32,
        EnforcementMode::VisibilityOnly => core_v1::EnforcementMode::VisibilityOnly as i32,
        EnforcementMode::ObserveOnly => core_v1::EnforcementMode::ObserveOnly as i32,
        EnforcementMode::Unenforced => core_v1::EnforcementMode::Unenforced as i32,
    }
}
