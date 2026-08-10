//! NVIDIA adapter host: the only CYRENE component allowed to interrogate this
//! vendor's command-line tools, sysfs layout, and device nodes.

pub mod discovery;

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    AcceleratorDevice, AcceleratorKind, AcceleratorLinkType, AcceleratorProvider,
    AcceleratorVendor, DeviceBinding, EnforcementMode, HealthReport, HostInventoryProvider,
    ProviderError,
};
use cy_proto::{core_v1, hardware_v1};

pub const PROTOCOL_VERSION: u32 = 1;

/// Handle one bounded local-adapter request.  The caller owns transport and
/// process supervision; this function has no access to the Kernel process.
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
                            devices: inventory.devices.iter().map(device_to_proto).collect(),
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
                        },
                    )),
                },
                Err(error) => provider_error_response(&adapter_id, error),
            }
        }
        Some(hardware_v1::adapter_request::Body::CreateBinding(request)) => {
            match create_binding(provider, &request) {
                Ok(binding) => hardware_v1::AdapterResponse {
                    protocol_version: PROTOCOL_VERSION,
                    adapter_id,
                    body: Some(hardware_v1::adapter_response::Body::Binding(
                        binding_to_proto(binding),
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

fn create_binding(
    provider: &discovery::NvidiaSmiProvider,
    request: &hardware_v1::CreateBindingRequest,
) -> Result<DeviceBinding, ProviderError> {
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
    let device = inventory
        .devices
        .iter()
        .find(|device| device.device_id == request.device_id)
        .ok_or_else(|| {
            ProviderError::new(
                provider.adapter_id(),
                "DEVICE_NOT_FOUND",
                "device is absent",
            )
        })?;
    provider.create_binding(device)
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

fn device_to_proto(device: &AcceleratorDevice) -> core_v1::AcceleratorDevice {
    core_v1::AcceleratorDevice {
        device_id: device.device_id.clone(),
        kind: accelerator_kind_to_proto(device.kind),
        vendor: accelerator_vendor_to_proto(device.vendor),
        other_vendor_id: String::new(),
        device_family: device.device_family.clone(),
        pci_address: device.pci_address.clone().unwrap_or_default(),
        total_memory_bytes: device.total_memory_bytes.unwrap_or_default(),
        allocatable_memory_bytes: device.allocatable_memory_bytes.unwrap_or_default(),
        features: device.features.clone(),
        partitions: Vec::new(),
        health: Some(health_to_proto(&device.health)),
        numa_node: device.numa_node,
        links: device
            .links
            .iter()
            .map(|link| core_v1::AcceleratorLink {
                peer_device_id: link.peer_device_id.clone(),
                link_type: accelerator_link_type_to_proto(link.link_type),
                link_count: link.link_count.unwrap_or_default(),
                width: link.width.unwrap_or_default(),
                bandwidth_bytes_per_second: link.bandwidth_bytes_per_second.unwrap_or_default(),
                stable: link.stable,
            })
            .collect(),
    }
}

fn binding_to_proto(binding: DeviceBinding) -> hardware_v1::DeviceBinding {
    hardware_v1::DeviceBinding {
        device_id: binding.device_id,
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
    }
}

fn health_to_proto(health: &HealthReport) -> core_v1::HealthReport {
    let status = match health.healthy {
        Some(true) => core_v1::HealthStatus::Healthy,
        Some(false) => core_v1::HealthStatus::Unhealthy,
        None => core_v1::HealthStatus::Unknown,
    };
    core_v1::HealthReport {
        status: status as i32,
        reason_code: health.reason_code.clone(),
        summary: health.summary.clone(),
    }
}

fn enforcement_to_proto(report: cy_kernel_api::EnforcementReport) -> core_v1::EnforcementReport {
    core_v1::EnforcementReport {
        resource_kind: core_v1::ResourceKind::Accelerator as i32,
        mode: enforcement_mode_to_proto(report.mode),
        adapter_id: report.adapter_id,
        reason_code: report.reason_code,
    }
}

fn accelerator_kind_to_proto(value: AcceleratorKind) -> i32 {
    match value {
        AcceleratorKind::Gpu => core_v1::AcceleratorKind::Gpu as i32,
        AcceleratorKind::Npu => core_v1::AcceleratorKind::Npu as i32,
        AcceleratorKind::Tpu => core_v1::AcceleratorKind::Tpu as i32,
        AcceleratorKind::Other => core_v1::AcceleratorKind::Other as i32,
    }
}
fn accelerator_vendor_to_proto(value: AcceleratorVendor) -> i32 {
    match value {
        AcceleratorVendor::Nvidia => core_v1::AcceleratorVendor::Nvidia as i32,
        AcceleratorVendor::Amd => core_v1::AcceleratorVendor::Amd as i32,
        AcceleratorVendor::HuaweiAscend => core_v1::AcceleratorVendor::HuaweiAscend as i32,
        AcceleratorVendor::Intel => core_v1::AcceleratorVendor::Intel as i32,
        AcceleratorVendor::Other => core_v1::AcceleratorVendor::Other as i32,
    }
}
fn accelerator_link_type_to_proto(value: AcceleratorLinkType) -> i32 {
    match value {
        AcceleratorLinkType::Pcie => core_v1::AcceleratorLinkType::Pcie as i32,
        AcceleratorLinkType::Nvlink => core_v1::AcceleratorLinkType::Nvlink as i32,
        AcceleratorLinkType::Xgmi => core_v1::AcceleratorLinkType::Xgmi as i32,
        AcceleratorLinkType::Other => core_v1::AcceleratorLinkType::Other as i32,
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
