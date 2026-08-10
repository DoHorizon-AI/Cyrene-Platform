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
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic::{self, Resource, ResourceState},
    CapabilityFact, DeviceBinding, DeviceNode, EnforcementMode, EnforcementReport, HealthReport,
    HostInventoryProvider, InventorySnapshot, NodeCapabilities, ProviderError, ResourceProvider,
};
use cy_proto::{core_v1, hardware_v1, semantic_v1};
use prost::Message;

const PROTOCOL_VERSION: u32 = 2;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Optional UDS peer identity constraint from static node configuration. The
/// Kernel verifies it after connect, before sending an adapter request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerCredentialExpectation {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl PeerCredentialExpectation {
    pub fn is_configured(self) -> bool {
        self.uid.is_some() || self.gid.is_some()
    }

    #[cfg(any(test, target_os = "linux"))]
    fn verify(
        self,
        adapter_id: &str,
        actual_uid: u32,
        actual_gid: u32,
    ) -> Result<(), ProviderError> {
        if self.uid.is_some_and(|uid| uid != actual_uid)
            || self.gid.is_some_and(|gid| gid != actual_gid)
        {
            return Err(ProviderError::new(
                adapter_id,
                "ADAPTER_PEER_CREDENTIAL_MISMATCH",
                "UDS peer credentials do not match the configured adapter identity",
            ));
        }
        Ok(())
    }
}

/// A single external adapter endpoint.  The adapter process remains the fault
/// boundary; this client does not dynamically load any vendor implementation.
#[derive(Debug, Clone)]
pub struct UdsHardwareAdapterClient {
    adapter_id: String,
    socket_path: PathBuf,
    timeout: Duration,
    peer_credentials: PeerCredentialExpectation,
}

/// Static Kernel configuration for one external hardware Adapter endpoint.
/// The ID is protocol identity, not a vendor name: examples include
/// `nvidia`, `amd-gpu`, `lab-accelerator-a`, or a future virtual partitioner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareAdapterEndpoint {
    pub adapter_id: String,
    pub socket_path: PathBuf,
    pub timeout: Duration,
    pub peer_credentials: PeerCredentialExpectation,
}

impl HardwareAdapterEndpoint {
    pub fn new(adapter_id: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            socket_path: socket_path.into(),
            timeout: Duration::from_secs(2),
            peer_credentials: PeerCredentialExpectation::default(),
        }
    }

    pub fn with_peer_credentials(mut self, peer_credentials: PeerCredentialExpectation) -> Self {
        self.peer_credentials = peer_credentials;
        self
    }
}

/// Common port implemented by any external hardware adapter client. It lets
/// the Kernel aggregate several UDS Sidecars without learning their vendors.
pub trait HardwareAdapter: HostInventoryProvider + ResourceProvider {}

impl<T> HardwareAdapter for T where T: HostInventoryProvider + ResourceProvider {}

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
            peer_credentials: PeerCredentialExpectation::default(),
        }
    }

    /// Set the bounded per-request adapter round-trip timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_peer_credentials(mut self, peer_credentials: PeerCredentialExpectation) -> Self {
        self.peer_credentials = peer_credentials;
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
        let response = exchange(
            &self.adapter_id,
            &self.socket_path,
            self.timeout,
            self.peer_credentials,
            &payload,
        )?;
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
                ensure_inventory_fresh(&self.adapter_id, &inventory)?;
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
            resources: inventory
                .resources
                .into_iter()
                .map(|resource| {
                    let mut resource = resource_from_proto(resource)?;
                    resource.provider.id = adapter_id.clone();
                    Ok(resource)
                })
                .collect::<Result<Vec<_>, ProviderError>>()?,
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

impl ResourceProvider for UdsHardwareAdapterClient {
    fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
        Ok(HostInventoryProvider::probe_inventory(self)?.resources)
    }

    fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
        self.create_binding_for_generation(resource, 0)
    }

    #[allow(deprecated)]
    fn create_binding_for_generation(
        &self,
        resource: &Resource,
        expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        let response = self.call(hardware_v1::adapter_request::Body::CreateBinding(
            hardware_v1::CreateBindingRequest {
                device_id: resource.identity.id.clone(),
                expected_inventory_generation,
                resource: Some(identity_to_proto(&resource.identity)),
            },
        ))?;
        match response.body {
            Some(hardware_v1::adapter_response::Body::Binding(binding)) => {
                let returned = binding.resource.as_ref();
                let returned_id = returned
                    .map(|identity| identity.id.as_str())
                    .filter(|id| !id.is_empty())
                    .unwrap_or(&binding.device_id);
                if returned_id != resource.identity.id
                    || returned.is_some_and(|identity| {
                        identity.generation != 0
                            && identity.generation != resource.identity.generation
                    })
                {
                    return Err(ProviderError::new(
                        &self.adapter_id,
                        "ADAPTER_BINDING_RESOURCE_MISMATCH",
                        "adapter returned a binding for another resource incarnation",
                    ));
                }
                Ok(DeviceBinding {
                    resource_id: returned_id.to_string(),
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

    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
        HostInventoryProvider::probe_inventory(self)?
            .resources
            .into_iter()
            .find(|resource| resource.identity.id == resource_id)
            .map(|resource| HealthReport {
                healthy: Some(resource.state == ResourceState::Ready),
                reason_code: resource.reason_code,
                summary: resource.summary,
            })
            .ok_or_else(|| {
                ProviderError::new(
                    &self.adapter_id,
                    "RESOURCE_NOT_FOUND",
                    "resource is absent from adapter inventory",
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
                    .with_timeout(endpoint.timeout)
                    .with_peer_credentials(endpoint.peer_credentials),
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
        let mut resources = Vec::new();
        let mut facts = Vec::new();
        let mut enforcement = Vec::new();
        let mut seen_resource_ids = BTreeSet::new();
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
            for mut resource in snapshot.resources {
                if !seen_resource_ids.insert(resource.identity.id.clone()) {
                    return Err(ProviderError::new(
                        "hardware-adapter-registry",
                        "ADAPTER_RESOURCE_ID_COLLISION",
                        &resource.identity.id,
                    ));
                }
                // Registry configuration, not adapter-supplied data, is the
                // provenance authority used for binding routing.
                resource.provider.id = adapter_id.clone();
                resources.push(resource);
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
            resources,
            capabilities: NodeCapabilities {
                ready,
                facts,
                enforcement,
            },
        })
    }

    fn adapter_for_resource(
        &self,
        resource: &Resource,
    ) -> Result<&Arc<dyn HardwareAdapter>, ProviderError> {
        if resource.provider.id.is_empty() {
            return Err(ProviderError::new(
                "hardware-adapter-registry",
                "RESOURCE_PROVENANCE_MISSING",
                &resource.identity.id,
            ));
        }
        self.adapters.get(&resource.provider.id).ok_or_else(|| {
            ProviderError::new(
                "hardware-adapter-registry",
                "RESOURCE_PROVIDER_NOT_REGISTERED",
                &resource.provider.id,
            )
        })
    }
}

impl HostInventoryProvider for UdsHardwareAdapterRegistry {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.aggregate_inventory()
    }
}

impl ResourceProvider for UdsHardwareAdapterRegistry {
    fn adapter_id(&self) -> &str {
        "uds-hardware-adapter-registry"
    }

    fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
        Ok(self.aggregate_inventory()?.resources)
    }

    fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
        self.create_binding_for_generation(resource, 0)
    }

    fn create_binding_for_generation(
        &self,
        resource: &Resource,
        expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        let binding = self
            .adapter_for_resource(resource)?
            .create_binding_for_generation(resource, expected_inventory_generation)?;
        if binding.resource_id != resource.identity.id || binding.adapter_id != resource.provider.id
        {
            return Err(ProviderError::new(
                "hardware-adapter-registry",
                "ADAPTER_BINDING_PROVENANCE_MISMATCH",
                &resource.identity.id,
            ));
        }
        Ok(binding)
    }

    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
        let resource = self
            .aggregate_inventory()?
            .resources
            .into_iter()
            .find(|resource| resource.identity.id == resource_id)
            .ok_or_else(|| {
                ProviderError::new(
                    "hardware-adapter-registry",
                    "RESOURCE_NOT_FOUND",
                    resource_id,
                )
            })?;
        self.adapter_for_resource(&resource)?
            .read_health(resource_id)
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
    peer_credentials: PeerCredentialExpectation,
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
        verify_connected_peer(adapter_id, &stream, peer_credentials)?;
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
        let _ = (socket_path, timeout, peer_credentials, payload);
        Err(ProviderError::new(
            adapter_id,
            "ADAPTER_UDS_UNSUPPORTED",
            "Unix domain sockets require a Unix Kernel host",
        ))
    }
}

#[cfg(unix)]
fn verify_connected_peer(
    adapter_id: &str,
    stream: &std::os::unix::net::UnixStream,
    expected: PeerCredentialExpectation,
) -> Result<(), ProviderError> {
    if !expected.is_configured() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let credentials =
            nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                .map_err(|error| {
                    ProviderError::new(
                        adapter_id,
                        "ADAPTER_PEER_CREDENTIAL_UNAVAILABLE",
                        &error.to_string(),
                    )
                })?;
        return expected.verify(adapter_id, credentials.uid(), credentials.gid());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(ProviderError::new(
            adapter_id,
            "ADAPTER_PEER_CREDENTIAL_UNSUPPORTED",
            "configured UDS peer credential checks require a Linux Kernel host",
        ))
    }
}

fn ensure_inventory_fresh(
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
        state: resource_state_from_proto(resource.state),
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

fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn identity_from_proto(identity: semantic_v1::Identity) -> semantic::Identity {
    semantic::Identity {
        id: identity.id,
        generation: identity.generation,
    }
}

fn resource_state_from_proto(value: i32) -> ResourceState {
    match semantic_v1::ResourceState::try_from(value)
        .unwrap_or(semantic_v1::ResourceState::Unavailable)
    {
        semantic_v1::ResourceState::Ready => ResourceState::Ready,
        semantic_v1::ResourceState::Degraded => ResourceState::Degraded,
        _ => ResourceState::Unavailable,
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
#[allow(deprecated)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone)]
    struct FakeAdapter {
        id: String,
        device_id: String,
    }

    impl FakeAdapter {
        fn resource(&self) -> Resource {
            Resource {
                identity: semantic::Identity {
                    id: self.device_id.clone(),
                    generation: 1,
                },
                provider: semantic::Identity {
                    id: self.id.clone(),
                    generation: 1,
                },
                resource_class: "accelerator".to_string(),
                capabilities: Vec::new(),
                capacity: BTreeMap::new(),
                attributes: BTreeMap::new(),
                state: ResourceState::Ready,
                reason_code: "test".to_string(),
                summary: "healthy".to_string(),
                links: Vec::new(),
            }
        }
    }

    impl HostInventoryProvider for FakeAdapter {
        fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                generation: 1,
                resources: vec![self.resource()],
                capabilities: NodeCapabilities {
                    ready: true,
                    facts: Vec::new(),
                    enforcement: Vec::new(),
                },
            })
        }
    }

    impl ResourceProvider for FakeAdapter {
        fn adapter_id(&self) -> &str {
            &self.id
        }

        fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
            Ok(vec![self.resource()])
        }

        fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
            if resource.identity.id != self.device_id {
                return Err(ProviderError::new(
                    &self.id,
                    "RESOURCE_NOT_FOUND",
                    &resource.identity.id,
                ));
            }
            Ok(DeviceBinding {
                resource_id: resource.identity.id.clone(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::Unenforced,
                adapter_id: self.id.clone(),
                reason_code: "TEST".to_string(),
            })
        }

        fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
            if resource_id != self.device_id {
                return Err(ProviderError::new(
                    &self.id,
                    "RESOURCE_NOT_FOUND",
                    resource_id,
                ));
            }
            Ok(HealthReport {
                healthy: Some(true),
                reason_code: "TEST".to_string(),
                summary: "healthy".to_string(),
            })
        }
    }

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"adapter").unwrap();
        assert_eq!(read_frame(wire.as_slice()).unwrap(), b"adapter");
    }

    #[test]
    fn unknown_resource_state_remains_unavailable() {
        assert_eq!(
            resource_state_from_proto(semantic_v1::ResourceState::Unspecified as i32),
            ResourceState::Unavailable
        );
    }

    #[test]
    fn peer_credential_policy_rejects_a_mismatched_adapter_peer() {
        let policy = PeerCredentialExpectation {
            uid: Some(1000),
            gid: Some(2000),
        };
        assert!(policy.verify("test", 1000, 2000).is_ok());
        assert_eq!(
            policy.verify("test", 1001, 2000).unwrap_err().reason_code,
            "ADAPTER_PEER_CREDENTIAL_MISMATCH"
        );
    }

    #[test]
    fn expired_or_missing_inventory_facts_fail_closed() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64;
        let valid = hardware_v1::HardwareInventory {
            generation: 1,
            devices: Vec::new(),
            facts: Vec::new(),
            enforcement: Vec::new(),
            sampled_at: Some(prost_types::Timestamp {
                seconds: now.saturating_sub(1),
                nanos: 0,
            }),
            expires_at: Some(prost_types::Timestamp {
                seconds: now.saturating_add(30),
                nanos: 0,
            }),
            resources: Vec::new(),
        };
        assert!(ensure_inventory_fresh("test", &valid).is_ok());
        let expired = hardware_v1::HardwareInventory {
            expires_at: Some(prost_types::Timestamp {
                seconds: now.saturating_sub(1),
                nanos: 0,
            }),
            ..valid
        };
        assert_eq!(
            ensure_inventory_fresh("test", &expired)
                .unwrap_err()
                .reason_code,
            "ADAPTER_FACT_EXPIRED"
        );
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
        assert_eq!(inventory.resources.len(), 2);
        let resource = inventory
            .resources
            .iter()
            .find(|resource| resource.identity.id == "gpu-b")
            .unwrap();
        assert_eq!(resource.provider.id, "adapter_b");
        let binding = registry
            .create_binding_for_generation(resource, inventory.generation)
            .unwrap();
        assert_eq!(binding.adapter_id, "adapter_b");
        assert_eq!(binding.resource_id, "gpu-b");
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
            "ADAPTER_RESOURCE_ID_COLLISION"
        );
    }
}
