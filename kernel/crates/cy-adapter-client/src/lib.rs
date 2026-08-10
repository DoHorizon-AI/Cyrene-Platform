//! Generic Kernel-side client for external hardware adapter processes.
//!
//! This crate owns only a bounded, versioned Unix-domain-socket exchange.  It
//! contains no driver command, vendor library, sysfs probe, or device-node
//! knowledge.  A disconnected adapter is reported as a provider failure so the
//! caller can stop issuing new leases while preserving its existing state.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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

/// Static Kernel configuration for one external hardware Adapter endpoint.
/// The ID is protocol identity, not a vendor name: examples include
/// `nvidia`, `amd-gpu`, `lab-accelerator-a`, or a future virtual partitioner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareAdapterEndpoint {
    pub adapter_id: String,
    pub socket_path: PathBuf,
    pub timeout: Duration,
}

impl HardwareAdapterEndpoint {
    pub fn new(adapter_id: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(2),
        }
    }
}

/// Common port implemented by any external hardware adapter client. It lets
/// the Kernel aggregate several UDS Sidecars without learning their vendors.
pub trait HardwareAdapter: HostInventoryProvider + AcceleratorProvider {}

impl<T> HardwareAdapter for T where T: HostInventoryProvider + AcceleratorProvider {}

/// Generic registry/aggregator for independent external hardware adapters.
/// It owns routing provenance and a Kernel-local monotonic aggregate generation;
/// it has no driver, topology, or vendor policy.
pub struct UdsHardwareAdapterRegistry {
    adapters: BTreeMap<String, Arc<dyn HardwareAdapter>>,
    generation: Mutex<AggregateGeneration>,
}

#[derive(Debug, Default)]
struct AggregateGeneration {
    generation: u64,
    fingerprint: String,
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
        if !response.adapter_id.is_empty() && response.adapter_id != self.adapter_id {
            return Err(ProviderError::new(
                &self.adapter_id,
                "ADAPTER_IDENTITY_MISMATCH",
                "UDS endpoint returned a different adapter_id",
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
        match response.body {
            Some(hardware_v1::adapter_response::Body::Inventory(inventory)) => {
                Ok((self.adapter_id.clone(), inventory))
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
        let (adapter_id, inventory) = self.inventory_response()?;
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
                .map(|device| {
                    let mut device = device_from_proto(device);
                    device.adapter_id = adapter_id.clone();
                    device
                })
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
        match response.body {
            Some(hardware_v1::adapter_response::Body::Binding(binding)) => {
                if binding.device_id != device.device_id {
                    return Err(ProviderError::new(
                        &self.adapter_id,
                        "ADAPTER_BINDING_DEVICE_MISMATCH",
                        "adapter returned a binding for another device",
                    ));
                }
                Ok(DeviceBinding {
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
                    adapter_id: self.adapter_id.clone(),
                    reason_code: binding.reason_code,
                })
            }
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

impl UdsHardwareAdapterRegistry {
    /// Builds a registry from explicit UDS endpoint configuration. At least one
    /// endpoint and unique, safe adapter IDs are required; the Kernel never
    /// discovers or loads adapters dynamically.
    pub fn from_endpoints(
        endpoints: impl IntoIterator<Item = HardwareAdapterEndpoint>,
    ) -> Result<Self, ProviderError> {
        let adapters = endpoints
            .into_iter()
            .map(|endpoint| {
                if !safe_adapter_id(&endpoint.adapter_id) || !endpoint.socket_path.is_absolute() {
                    return Err(ProviderError::new(
                        "hardware-adapter-registry",
                        "ADAPTER_ENDPOINT_INVALID",
                        "adapter IDs must be safe and UDS paths must be absolute",
                    ));
                }
                let adapter: Arc<dyn HardwareAdapter> = Arc::new(
                    UdsHardwareAdapterClient::new(
                        endpoint.adapter_id.clone(),
                        endpoint.socket_path,
                    )
                    .with_timeout(endpoint.timeout),
                );
                Ok((endpoint.adapter_id, adapter))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Self::from_adapters(adapters)
    }

    /// Dependency-injection constructor used by tests and by future non-UDS
    /// test harnesses. Production Kernel composition uses [`Self::from_endpoints`].
    pub fn from_adapters(
        adapters: impl IntoIterator<Item = (String, Arc<dyn HardwareAdapter>)>,
    ) -> Result<Self, ProviderError> {
        let mut registered = BTreeMap::new();
        for (adapter_id, adapter) in adapters {
            if !safe_adapter_id(&adapter_id)
                || registered.insert(adapter_id.clone(), adapter).is_some()
            {
                return Err(ProviderError::new(
                    "hardware-adapter-registry",
                    "ADAPTER_REGISTRATION_INVALID",
                    "adapter IDs must be safe and unique",
                ));
            }
        }
        if registered.is_empty() {
            return Err(ProviderError::new(
                "hardware-adapter-registry",
                "ADAPTER_REGISTRATION_EMPTY",
                "at least one external hardware adapter is required",
            ));
        }
        Ok(Self {
            adapters: registered,
            generation: Mutex::new(AggregateGeneration::default()),
        })
    }

    fn aggregate_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        let mut devices = Vec::new();
        let mut facts = Vec::new();
        let mut enforcement = Vec::new();
        let mut seen_device_ids = BTreeSet::new();
        let mut fingerprint_parts = Vec::new();
        let mut ready = true;

        for (adapter_id, adapter) in &self.adapters {
            let snapshot =
                HostInventoryProvider::probe_inventory(adapter.as_ref()).map_err(|error| {
                    ProviderError::new(
                        "hardware-adapter-registry",
                        "ADAPTER_UNAVAILABLE",
                        &format!("{adapter_id}: {}", error.message),
                    )
                })?;
            if snapshot.generation == 0 {
                return Err(ProviderError::new(
                    "hardware-adapter-registry",
                    "ADAPTER_GENERATION_INVALID",
                    adapter_id,
                ));
            }
            ready &= snapshot.capabilities.ready;
            fingerprint_parts.push(format!("{adapter_id}:{snapshot:?}"));
            for mut device in snapshot.devices {
                if !seen_device_ids.insert(device.device_id.clone()) {
                    return Err(ProviderError::new(
                        "hardware-adapter-registry",
                        "ADAPTER_DEVICE_ID_COLLISION",
                        &device.device_id,
                    ));
                }
                // Registry configuration, not adapter-supplied data, is the
                // provenance authority used for binding routing.
                device.adapter_id = adapter_id.clone();
                devices.push(device);
            }
            facts.extend(snapshot.capabilities.facts.into_iter().map(|mut fact| {
                fact.name = format!("adapter.{adapter_id}.{}", fact.name);
                fact
            }));
            enforcement.extend(
                snapshot
                    .capabilities
                    .enforcement
                    .into_iter()
                    .map(|mut report| {
                        report.adapter_id = adapter_id.clone();
                        report
                    }),
            );
        }

        let fingerprint = fingerprint_parts.join("\n");
        let generation = {
            let mut state = self
                .generation
                .lock()
                .expect("aggregate generation lock poisoned");
            if state.generation == 0 || state.fingerprint != fingerprint {
                state.generation = state.generation.saturating_add(1).max(1);
                state.fingerprint = fingerprint;
            }
            state.generation
        };
        Ok(InventorySnapshot {
            generation,
            devices,
            capabilities: NodeCapabilities {
                ready,
                facts,
                enforcement,
            },
        })
    }

    fn adapter_for_device(
        &self,
        device: &AcceleratorDevice,
    ) -> Result<&Arc<dyn HardwareAdapter>, ProviderError> {
        if device.adapter_id.is_empty() {
            return Err(ProviderError::new(
                "hardware-adapter-registry",
                "DEVICE_PROVENANCE_MISSING",
                &device.device_id,
            ));
        }
        self.adapters.get(&device.adapter_id).ok_or_else(|| {
            ProviderError::new(
                "hardware-adapter-registry",
                "DEVICE_ADAPTER_NOT_REGISTERED",
                &device.adapter_id,
            )
        })
    }
}

impl HostInventoryProvider for UdsHardwareAdapterRegistry {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.aggregate_inventory()
    }
}

impl AcceleratorProvider for UdsHardwareAdapterRegistry {
    fn adapter_id(&self) -> &str {
        "uds-hardware-adapter-registry"
    }

    fn probe_inventory(&self) -> Result<Vec<AcceleratorDevice>, ProviderError> {
        Ok(self.aggregate_inventory()?.devices)
    }

    fn create_binding(&self, device: &AcceleratorDevice) -> Result<DeviceBinding, ProviderError> {
        self.create_binding_for_generation(device, 0)
    }

    fn create_binding_for_generation(
        &self,
        device: &AcceleratorDevice,
        expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        let binding = self
            .adapter_for_device(device)?
            .create_binding_for_generation(device, expected_inventory_generation)?;
        if binding.device_id != device.device_id || binding.adapter_id != device.adapter_id {
            return Err(ProviderError::new(
                "hardware-adapter-registry",
                "ADAPTER_BINDING_PROVENANCE_MISMATCH",
                &device.device_id,
            ));
        }
        Ok(binding)
    }

    fn read_health(&self, device_id: &str) -> Result<HealthReport, ProviderError> {
        let device = self
            .aggregate_inventory()?
            .devices
            .into_iter()
            .find(|device| device.device_id == device_id)
            .ok_or_else(|| {
                ProviderError::new("hardware-adapter-registry", "DEVICE_NOT_FOUND", device_id)
            })?;
        self.adapter_for_device(&device)?.read_health(device_id)
    }
}

fn safe_adapter_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
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
        adapter_id: String::new(),
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
    use std::sync::Arc;

    #[derive(Clone)]
    struct FakeAdapter {
        id: String,
        device_id: String,
    }

    impl FakeAdapter {
        fn device(&self) -> AcceleratorDevice {
            AcceleratorDevice {
                device_id: self.device_id.clone(),
                adapter_id: self.id.clone(),
                kind: AcceleratorKind::Gpu,
                vendor: AcceleratorVendor::Other,
                device_family: "test".to_string(),
                pci_address: None,
                numa_node: None,
                total_memory_bytes: None,
                allocatable_memory_bytes: None,
                features: Vec::new(),
                device_nodes: Vec::new(),
                links: Vec::new(),
                health: HealthReport {
                    healthy: Some(true),
                    reason_code: "TEST".to_string(),
                    summary: "healthy".to_string(),
                },
            }
        }
    }

    impl HostInventoryProvider for FakeAdapter {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                devices: vec![self.device()],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
        }
    }

    impl AcceleratorProvider for FakeAdapter {
        fn adapter_id(&self) -> &str {
            &self.id
        }

        fn probe_inventory(&self) -> Result<Vec<AcceleratorDevice>, ProviderError> {
            Ok(vec![self.device()])
        }

        fn create_binding(
            &self,
            device: &AcceleratorDevice,
        ) -> Result<DeviceBinding, ProviderError> {
            if device.device_id != self.device_id {
                return Err(ProviderError::new(
                    &self.id,
                    "DEVICE_NOT_FOUND",
                    &device.device_id,
                ));
            }
            Ok(DeviceBinding {
                device_id: device.device_id.clone(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Unenforced,
                adapter_id: self.id.clone(),
                reason_code: "TEST".to_string(),
            })
        }

        fn read_health(&self, device_id: &str) -> Result<HealthReport, ProviderError> {
            if device_id != self.device_id {
                return Err(ProviderError::new(&self.id, "DEVICE_NOT_FOUND", device_id));
            }
            Ok(self.device().health)
        }
    }

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

    #[test]
    fn registry_aggregates_adapters_and_routes_binding_by_provenance() {
        let registry = UdsHardwareAdapterRegistry::from_adapters(vec![
            (
                "adapter_a".to_string(),
                Arc::new(FakeAdapter {
                    id: "adapter_a".to_string(),
                    device_id: "gpu-a".to_string(),
                }) as Arc<dyn HardwareAdapter>,
            ),
            (
                "adapter_b".to_string(),
                Arc::new(FakeAdapter {
                    id: "adapter_b".to_string(),
                    device_id: "gpu-b".to_string(),
                }) as Arc<dyn HardwareAdapter>,
            ),
        ])
        .unwrap();
        let inventory = HostInventoryProvider::probe_inventory(&registry).unwrap();
        assert_eq!(inventory.devices.len(), 2);
        let device = inventory
            .devices
            .iter()
            .find(|device| device.device_id == "gpu-b")
            .unwrap();
        assert_eq!(device.adapter_id, "adapter_b");
        let binding = registry
            .create_binding_for_generation(device, inventory.generation)
            .unwrap();
        assert_eq!(binding.adapter_id, "adapter_b");
        assert_eq!(binding.device_id, "gpu-b");
    }

    #[test]
    fn registry_rejects_duplicate_device_ids_across_adapters() {
        let registry = UdsHardwareAdapterRegistry::from_adapters(vec![
            (
                "adapter_a".to_string(),
                Arc::new(FakeAdapter {
                    id: "adapter_a".to_string(),
                    device_id: "same-device".to_string(),
                }) as Arc<dyn HardwareAdapter>,
            ),
            (
                "adapter_b".to_string(),
                Arc::new(FakeAdapter {
                    id: "adapter_b".to_string(),
                    device_id: "same-device".to_string(),
                }) as Arc<dyn HardwareAdapter>,
            ),
        ])
        .unwrap();
        assert_eq!(
            HostInventoryProvider::probe_inventory(&registry)
                .unwrap_err()
                .reason_code,
            "ADAPTER_DEVICE_ID_COLLISION"
        );
    }
}
