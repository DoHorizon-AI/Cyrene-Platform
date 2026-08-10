//! Generic Kernel-side client for external hardware adapter processes.
//!
//! This crate owns only a bounded, versioned Unix-domain-socket exchange.  It
//! contains no driver command, vendor library, sysfs probe, or device-node
//! knowledge.  A disconnected adapter is reported as a provider failure so the
//! caller can stop issuing new leases while preserving its existing state.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use cy_kernel_api::{
    AcceleratorDevice, AcceleratorKind, AcceleratorLink, AcceleratorLinkType, AcceleratorProvider,
    AcceleratorVendor, CapabilityFact, DeviceBinding, DeviceNode, EnforcementMode,
    EnforcementReport, HealthReport, HostInventoryProvider, InventorySnapshot, NodeCapabilities,
    ProviderError,
};
use cy_proto::{core_v1, hardware_v1};
use prost::Message;

const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// A single external adapter endpoint.  The adapter process remains the fault
/// boundary; this client does not dynamically load any vendor implementation.
#[derive(Debug, Clone)]
pub struct UdsHardwareAdapterClient {
    adapter_id: String,
    socket_path: PathBuf,
    timeout: Duration,
}

impl UdsHardwareAdapterClient {
    pub fn new(adapter_id: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(2),
        }
    }

    /// Set the bounded per-request adapter round-trip timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn call(
        &self,
        body: hardware_v1::adapter_request::Body,
    ) -> Result<hardware_v1::AdapterResponse, ProviderError> {
        let request = hardware_v1::AdapterRequest {
            protocol_version: PROTOCOL_VERSION,
            body: Some(body),
        };
        let payload = request.encode_to_vec();
        let response = exchange(&self.adapter_id, &self.socket_path, self.timeout, &payload)?;
        let response =
            hardware_v1::AdapterResponse::decode(response.as_slice()).map_err(|error| {
                ProviderError::new(
                    &self.adapter_id,
                    "ADAPTER_PROTOCOL_DECODE",
                    &error.to_string(),
                )
            })?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(ProviderError::new(
                &self.adapter_id,
                "ADAPTER_PROTOCOL_VERSION",
                "adapter returned an incompatible protocol version",
            ));
        }
        if let Some(hardware_v1::adapter_response::Body::Error(error)) = &response.body {
            return Err(ProviderError::new(
                if response.adapter_id.is_empty() {
                    &self.adapter_id
                } else {
                    &response.adapter_id
                },
                &error.reason_code,
                &error.message,
            ));
        }
        Ok(response)
    }

    fn inventory_response(
        &self,
    ) -> Result<(String, hardware_v1::HardwareInventory), ProviderError> {
        let response = self.call(hardware_v1::adapter_request::Body::GetInventory(
            hardware_v1::GetInventoryRequest {
                node_id: String::new(),
                known_generation: 0,
            },
        ))?;
        let adapter_id = if response.adapter_id.is_empty() {
            self.adapter_id.clone()
        } else {
            response.adapter_id
        };
        match response.body {
            Some(hardware_v1::adapter_response::Body::Inventory(inventory)) => {
                Ok((adapter_id, inventory))
            }
            _ => Err(ProviderError::new(
                &self.adapter_id,
                "ADAPTER_PROTOCOL_RESPONSE",
                "adapter did not return inventory",
            )),
        }
    }
}

impl HostInventoryProvider for UdsHardwareAdapterClient {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        let (_adapter_id, inventory) = self.inventory_response()?;
        let facts = inventory
            .facts
            .into_iter()
            .map(|fact| CapabilityFact {
                name: fact.name,
                available: fact.available,
                required: fact.required,
                detail: fact.detail,
            })
            .collect::<Vec<_>>();
        let ready = facts
            .iter()
            .filter(|fact| fact.required)
            .all(|fact| fact.available);
        Ok(InventorySnapshot {
            generation: inventory.generation,
            devices: inventory
                .devices
                .into_iter()
                .map(device_from_proto)
                .collect(),
            capabilities: NodeCapabilities {
                ready,
                facts,
                enforcement: inventory
                    .enforcement
                    .into_iter()
                    .map(enforcement_from_proto)
                    .collect(),
            },
        })
    }
}

impl AcceleratorProvider for UdsHardwareAdapterClient {
    fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    fn probe_inventory(&self) -> Result<Vec<AcceleratorDevice>, ProviderError> {
        Ok(HostInventoryProvider::probe_inventory(self)?.devices)
    }

    fn create_binding(&self, device: &AcceleratorDevice) -> Result<DeviceBinding, ProviderError> {
        self.create_binding_for_generation(device, 0)
    }

    fn create_binding_for_generation(
        &self,
        device: &AcceleratorDevice,
        expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        let response = self.call(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: device.device_id.clone(),
                expected_inventory_generation,
            },
        ))?;
        let adapter_id = if response.adapter_id.is_empty() {
            self.adapter_id.clone()
        } else {
            response.adapter_id
        };
        match response.body {
            Some(hardware_v1::adapter_response::Body::Binding(binding)) => Ok(DeviceBinding {
                device_id: binding.device_id,
                nodes: binding
                    .nodes
                    .into_iter()
                    .map(|node| DeviceNode {
                        path: PathBuf::from(node.path),
                        major: node.major,
                        minor: node.minor,
                        required: node.required,
                    })
                    .collect(),
                environment: binding.environment.into_iter().collect::<BTreeMap<_, _>>(),
                required_gids: binding.required_gids,
                enforcement: enforcement_mode_from_proto(binding.enforcement),
                adapter_id,
                reason_code: binding.reason_code,
            }),
            _ => Err(ProviderError::new(
                &self.adapter_id,
                "ADAPTER_PROTOCOL_RESPONSE",
                "adapter did not return a device binding",
            )),
        }
    }

    fn read_health(&self, device_id: &str) -> Result<HealthReport, ProviderError> {
        HostInventoryProvider::probe_inventory(self)?
            .devices
            .into_iter()
            .find(|device| device.device_id == device_id)
            .map(|device| device.health)
            .ok_or_else(|| {
                ProviderError::new(
                    &self.adapter_id,
                    "DEVICE_NOT_FOUND",
                    "device is absent from adapter inventory",
                )
            })
    }
}

fn exchange(
    adapter_id: &str,
    socket_path: &Path,
    timeout: Duration,
    payload: &[u8],
) -> Result<Vec<u8>, ProviderError> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(socket_path).map_err(|error| {
            ProviderError::new(
                adapter_id,
                "ADAPTER_UNAVAILABLE",
                &format!("{}: {error}", socket_path.display()),
            )
        })?;
        stream.set_read_timeout(Some(timeout)).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_CONFIG", &error.to_string())
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_CONFIG", &error.to_string())
        })?;
        write_frame(&mut stream, payload).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_WRITE", &error.to_string())
        })?;
        read_frame(&mut stream).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_READ", &error.to_string())
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (socket_path, timeout, payload);
        Err(ProviderError::new(
            adapter_id,
            "ADAPTER_UDS_UNSUPPORTED",
            "Unix domain sockets require a Unix Kernel host",
        ))
    }
}

pub fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)
}

pub fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter frame exceeds limit",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn device_from_proto(device: core_v1::AcceleratorDevice) -> AcceleratorDevice {
    AcceleratorDevice {
        device_id: device.device_id,
        kind: accelerator_kind_from_proto(device.kind),
        vendor: accelerator_vendor_from_proto(device.vendor),
        device_family: device.device_family,
        pci_address: (!device.pci_address.is_empty()).then_some(device.pci_address),
        numa_node: device.numa_node,
        total_memory_bytes: (device.total_memory_bytes != 0).then_some(device.total_memory_bytes),
        allocatable_memory_bytes: (device.allocatable_memory_bytes != 0)
            .then_some(device.allocatable_memory_bytes),
        features: device.features,
        device_nodes: Vec::new(),
        links: device
            .links
            .into_iter()
            .map(|link| AcceleratorLink {
                peer_device_id: link.peer_device_id,
                link_type: accelerator_link_type_from_proto(link.link_type),
                link_count: (link.link_count != 0).then_some(link.link_count),
                width: (link.width != 0).then_some(link.width),
                bandwidth_bytes_per_second: (link.bandwidth_bytes_per_second != 0)
                    .then_some(link.bandwidth_bytes_per_second),
                stable: link.stable,
            })
            .collect(),
        health: health_from_proto(device.health),
    }
}

fn health_from_proto(health: Option<core_v1::HealthReport>) -> HealthReport {
    let health = health.unwrap_or_default();
    let healthy = match core_v1::HealthStatus::try_from(health.status)
        .unwrap_or(core_v1::HealthStatus::Unknown)
    {
        core_v1::HealthStatus::Healthy => Some(true),
        core_v1::HealthStatus::Degraded | core_v1::HealthStatus::Unhealthy => Some(false),
        _ => None,
    };
    HealthReport {
        healthy,
        reason_code: health.reason_code,
        summary: health.summary,
    }
}

fn enforcement_from_proto(report: core_v1::EnforcementReport) -> EnforcementReport {
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

fn accelerator_kind_from_proto(value: i32) -> AcceleratorKind {
    match core_v1::AcceleratorKind::try_from(value).unwrap_or(core_v1::AcceleratorKind::Unspecified)
    {
        core_v1::AcceleratorKind::Gpu => AcceleratorKind::Gpu,
        core_v1::AcceleratorKind::Npu => AcceleratorKind::Npu,
        core_v1::AcceleratorKind::Tpu => AcceleratorKind::Tpu,
        _ => AcceleratorKind::Other,
    }
}
fn accelerator_vendor_from_proto(value: i32) -> AcceleratorVendor {
    match core_v1::AcceleratorVendor::try_from(value)
        .unwrap_or(core_v1::AcceleratorVendor::Unspecified)
    {
        core_v1::AcceleratorVendor::Nvidia => AcceleratorVendor::Nvidia,
        core_v1::AcceleratorVendor::Amd => AcceleratorVendor::Amd,
        core_v1::AcceleratorVendor::HuaweiAscend => AcceleratorVendor::HuaweiAscend,
        core_v1::AcceleratorVendor::Intel => AcceleratorVendor::Intel,
        _ => AcceleratorVendor::Other,
    }
}
fn accelerator_link_type_from_proto(value: i32) -> AcceleratorLinkType {
    match core_v1::AcceleratorLinkType::try_from(value)
        .unwrap_or(core_v1::AcceleratorLinkType::Unspecified)
    {
        core_v1::AcceleratorLinkType::Pcie => AcceleratorLinkType::Pcie,
        core_v1::AcceleratorLinkType::Nvlink => AcceleratorLinkType::Nvlink,
        core_v1::AcceleratorLinkType::Xgmi => AcceleratorLinkType::Xgmi,
        _ => AcceleratorLinkType::Other,
    }
}
fn enforcement_mode_from_proto(value: i32) -> EnforcementMode {
    match core_v1::EnforcementMode::try_from(value).unwrap_or(core_v1::EnforcementMode::Unspecified)
    {
        core_v1::EnforcementMode::Hard => EnforcementMode::Hard,
        core_v1::EnforcementMode::Soft => EnforcementMode::Soft,
        core_v1::EnforcementMode::VisibilityOnly => EnforcementMode::VisibilityOnly,
        core_v1::EnforcementMode::ObserveOnly => EnforcementMode::ObserveOnly,
        _ => EnforcementMode::Unenforced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"adapter").unwrap();
        assert_eq!(read_frame(wire.as_slice()).unwrap(), b"adapter");
    }

    #[test]
    fn unknown_health_remains_unknown() {
        assert_eq!(health_from_proto(None).healthy, None);
    }
}
