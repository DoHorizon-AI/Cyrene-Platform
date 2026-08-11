use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use cy_kernel_api::{
    semantic::{Resource, ResourceState},
    CapabilityFact, DeviceBinding, DeviceNode, HealthReport, HostInventoryProvider,
    InventorySnapshot, NodeCapabilities, ProviderError, ResourceProvider,
};
use cy_proto::hardware_v1;
use prost::Message;

use crate::{
    convert::{
        enforcement_from_proto, enforcement_mode_from_proto, ensure_inventory_fresh,
        identity_to_proto, resource_from_proto,
    },
    credential::PeerCredentialExpectation,
    transport::{exchange, PROTOCOL_VERSION},
};

/// Alias for `UdsHardwareAdapterClient` for naming consistency across kernel adapters.
pub type HostAdapterClient = UdsHardwareAdapterClient;

/// A single external adapter endpoint. The adapter process remains the fault
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

pub(crate) fn safe_adapter_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
