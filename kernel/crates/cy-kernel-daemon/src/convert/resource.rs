use std::collections::BTreeMap;

use cy_kernel_api::{
    semantic, CgroupLimits, DeviceBinding, EnforcementMode, ProviderError, ResourceRequest,
};
use cy_proto::{core_v1, semantic_v1};
use tonic::Status;

use super::common::to_semantic_proto_identity;

pub(crate) fn to_semantic_proto_resource(resource: &semantic::Resource) -> semantic_v1::Resource {
    semantic_v1::Resource {
        identity: Some(to_semantic_proto_identity(&resource.identity)),
        provider: Some(to_semantic_proto_identity(&resource.provider)),
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
        state: match resource.state {
            semantic::ResourceState::Ready => semantic_v1::ResourceState::Ready,
            semantic::ResourceState::Degraded => semantic_v1::ResourceState::Degraded,
            semantic::ResourceState::Unavailable => semantic_v1::ResourceState::Unavailable,
        } as i32,
        reason_code: resource.reason_code.clone(),
        summary: resource.summary.clone(),
        links: resource
            .links
            .iter()
            .map(|link| semantic_v1::TopologyLink {
                peer: Some(to_semantic_proto_identity(&link.peer)),
                kind: link.kind.clone(),
                properties: link.properties.clone().into_iter().collect(),
            })
            .collect(),
    }
}

pub(crate) fn semantic_query_from_proto(
    query: semantic_v1::ResourceQuery,
) -> Result<semantic::ResourceQuery, Status> {
    let query = semantic::ResourceQuery {
        resource_class: query.resource_class,
        count: query.count,
        required_capabilities: query
            .required_capabilities
            .into_iter()
            .map(|requirement| semantic::CapabilityRequirement {
                id: requirement.id,
                minimum_revision: requirement.minimum_revision,
                required_properties: requirement.required_properties.into_iter().collect(),
            })
            .collect(),
        minimum_capacity: query
            .minimum_capacity
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
    };
    query.validate().map_err(|error| {
        Status::invalid_argument(format!("{}: {}", error.reason_code, error.message))
    })?;
    Ok(query)
}

/// 将内部隔离执行模式转换为 Protobuf 协议枚举
pub(crate) fn to_proto_enforcement(mode: EnforcementMode) -> core_v1::EnforcementMode {
    match mode {
        EnforcementMode::Hard => core_v1::EnforcementMode::Hard,
        EnforcementMode::Soft => core_v1::EnforcementMode::Soft,
        EnforcementMode::VisibilityOnly => core_v1::EnforcementMode::VisibilityOnly,
        EnforcementMode::ObserveOnly => core_v1::EnforcementMode::ObserveOnly,
        EnforcementMode::Unenforced => core_v1::EnforcementMode::Unenforced,
    }
}

pub(crate) fn merge_bindings(bindings: Vec<DeviceBinding>) -> Result<DeviceBinding, ProviderError> {
    let Some(first) = bindings.first().cloned() else {
        return Ok(DeviceBinding {
            resource_id: "none".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Unenforced,
            adapter_id: "kernel-daemon".to_string(),
            reason_code: "NO_RESOURCE_BINDING".to_string(),
        });
    };
    let mut nodes = first.nodes;
    let mut environment = first.environment;
    let mut required_gids = first.required_gids;
    let enforcement = first.enforcement;
    let mut adapter_ids = vec![first.adapter_id.clone()];
    let mut resource_ids = vec![first.resource_id];
    for binding in bindings.into_iter().skip(1) {
        if binding.enforcement != enforcement {
            return Err(ProviderError::new(
                "kernel-daemon",
                "MIXED_RESOURCE_ENFORCEMENT",
                "a multi-resource binding must use one enforcement mode",
            ));
        }
        if !adapter_ids.contains(&binding.adapter_id) {
            adapter_ids.push(binding.adapter_id.clone());
        }
        resource_ids.push(binding.resource_id);
        for node in binding.nodes {
            if !nodes.iter().any(|existing| existing.path == node.path) {
                nodes.push(node);
            }
        }
        for (key, value) in binding.environment {
            if let Some(existing) = environment.get(&key) {
                if existing != &value {
                    return Err(ProviderError::new(
                        "kernel-daemon",
                        "CONFLICTING_RESOURCE_ENVIRONMENT",
                        &key,
                    ));
                }
            } else {
                environment.insert(key, value);
            }
        }
        for gid in binding.required_gids {
            if !required_gids.contains(&gid) {
                required_gids.push(gid);
            }
        }
    }
    adapter_ids.sort();
    Ok(DeviceBinding {
        resource_id: resource_ids.join(","),
        nodes,
        environment,
        required_gids,
        enforcement,
        adapter_id: adapter_ids.join(","),
        reason_code: "RESOURCE_BINDING_CREATED_BY_UDS_ADAPTERS".to_string(),
    })
}

pub(crate) fn resource_request(
    lease_name: &str,
    generation: u64,
    holder: semantic::Identity,
    expires_at_unix_ms: Option<u64>,
    requirements: &core_v1::ResourceRequirements,
) -> Result<ResourceRequest, Status> {
    let mut count = 0u32;
    let mut kind_capability: Option<String> = None;
    let mut required_capabilities = BTreeMap::<String, semantic::CapabilityRequirement>::new();
    let mut min_memory_bytes: Option<u64> = None;
    for accelerator in &requirements.accelerators {
        count = count
            .checked_add(accelerator.count)
            .ok_or_else(|| Status::invalid_argument("accelerator count overflow"))?;
        if accelerator.vendor != core_v1::AcceleratorVendor::Unspecified as i32
            || !accelerator.other_vendor_id.is_empty()
        {
            return Err(Status::failed_precondition(
                "legacy vendor selectors are not interpreted by Kernel; use AcquireLease with a namespaced Capability",
            ));
        }
        let requested_kind = match core_v1::AcceleratorKind::try_from(accelerator.kind)
            .map_err(|_| Status::invalid_argument("unknown accelerator kind"))?
        {
            core_v1::AcceleratorKind::Unspecified => "accelerator.compute",
            core_v1::AcceleratorKind::Gpu => "accelerator.kind.gpu",
            core_v1::AcceleratorKind::Npu => "accelerator.kind.npu",
            core_v1::AcceleratorKind::Tpu => "accelerator.kind.tpu",
            core_v1::AcceleratorKind::Other => "accelerator.kind.other",
        };
        if kind_capability
            .as_ref()
            .is_some_and(|current| current != requested_kind)
        {
            return Err(Status::invalid_argument(
                "one legacy request cannot mix accelerator kinds",
            ));
        }
        kind_capability = Some(requested_kind.to_string());
        for capability_id in &accelerator.required_features {
            if !capability_id.contains('.') {
                return Err(Status::failed_precondition(
                    "legacy bare feature names are ambiguous; use a namespaced Capability id",
                ));
            }
            required_capabilities.insert(
                capability_id.clone(),
                semantic::CapabilityRequirement {
                    id: capability_id.clone(),
                    minimum_revision: 1,
                    required_properties: BTreeMap::new(),
                },
            );
        }
        min_memory_bytes = match min_memory_bytes {
            Some(current) => Some(current.max(accelerator.min_memory_bytes_per_device)),
            None => Some(accelerator.min_memory_bytes_per_device),
        };
    }
    if count == 0 {
        return Err(Status::invalid_argument(
            "at least one accelerator resource is required",
        ));
    }
    let kind_capability = kind_capability.unwrap_or_else(|| "accelerator.compute".to_string());
    required_capabilities.insert(
        kind_capability.clone(),
        semantic::CapabilityRequirement {
            id: kind_capability,
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        },
    );
    let cpu_max_millicores = requirements
        .cpu
        .as_ref()
        .and_then(|cpu| (cpu.limit_millicores > 0).then_some(cpu.limit_millicores));
    if let Some(cpu) = requirements.cpu.as_ref() {
        if cpu.limit_millicores > 0
            && cpu.request_millicores > 0
            && cpu.request_millicores > cpu.limit_millicores
        {
            return Err(Status::invalid_argument(
                "cpu request_millicores cannot exceed limit_millicores",
            ));
        }
    }
    let memory_max_bytes = requirements
        .memory
        .as_ref()
        .and_then(|memory| (memory.limit_bytes > 0).then_some(memory.limit_bytes));
    if let Some(memory) = requirements.memory.as_ref() {
        if memory.limit_bytes > 0
            && memory.request_bytes > 0
            && memory.request_bytes > memory.limit_bytes
        {
            return Err(Status::invalid_argument(
                "memory request_bytes cannot exceed limit_bytes",
            ));
        }
    }
    Ok(ResourceRequest {
        lease_name: lease_name.to_string(),
        expected_inventory_generation: generation,
        holder,
        query: semantic::ResourceQuery {
            resource_class: "accelerator".to_string(),
            count,
            required_capabilities: required_capabilities.into_values().collect(),
            minimum_capacity: min_memory_bytes
                .filter(|value| *value > 0)
                .map(|value| {
                    BTreeMap::from([(
                        "memory.allocatable".to_string(),
                        semantic::Quantity {
                            value,
                            unit: "byte".to_string(),
                        },
                    )])
                })
                .unwrap_or_default(),
        },
        expires_at_unix_ms,
        limits: CgroupLimits {
            cpu_max_millicores,
            memory_max_bytes,
            cpuset_cpus: None,
        },
    })
}
