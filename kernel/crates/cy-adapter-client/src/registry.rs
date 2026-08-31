// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-adapter-client/src/registry.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use cy_kernel_api::{
    semantic::Resource, DeviceBinding, HealthReport, HostInventoryProvider, InventorySnapshot,
    NodeCapabilities, ProviderError, ResourceProvider,
};

use crate::client::{
    safe_adapter_id, HardwareAdapter, HardwareAdapterEndpoint, HardwareAdapterObservation,
    UdsHardwareAdapterClient,
};

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

    /// Collects each configured adapter independently. Callers that need
    /// provider lifecycle facts use this directly rather than the aggregate
    /// inventory generation retained for the resource ledger.
    pub fn adapter_observations(
        &self,
    ) -> BTreeMap<String, Result<HardwareAdapterObservation, ProviderError>> {
        self.adapters
            .iter()
            .map(|(adapter_id, adapter)| {
                let observation = adapter.observe_inventory().and_then(|mut observation| {
                    if observation.snapshot.generation == 0 {
                        return Err(ProviderError::new(
                            "hardware-adapter-registry",
                            "ADAPTER_GENERATION_INVALID",
                            adapter_id,
                        ));
                    }
                    // Registry configuration, not adapter-supplied data, is
                    // the provenance authority used for binding routing.
                    for resource in &mut observation.snapshot.resources {
                        resource.provider.id = adapter_id.clone();
                    }
                    Ok(observation)
                });
                (adapter_id.clone(), observation)
            })
            .collect()
    }

    /// Builds the existing allocation ledger view from a coherent collection
    /// of individual observations. A failed adapter still makes allocation
    /// facts unavailable, but does not hide successful provider observations.
    pub fn aggregate_observations(
        &self,
        observations: &BTreeMap<String, Result<HardwareAdapterObservation, ProviderError>>,
    ) -> Result<InventorySnapshot, ProviderError> {
        let mut resources = Vec::new();
        let mut facts = Vec::new();
        let mut enforcement = Vec::new();
        let mut seen_resource_ids = BTreeSet::new();
        let mut fingerprint_parts = Vec::new();
        let mut ready = true;

        for adapter_id in self.adapters.keys() {
            let snapshot = observations
                .get(adapter_id)
                .expect("observation is collected for every configured adapter")
                .as_ref()
                .map_err(|error| {
                    ProviderError::new(
                        "hardware-adapter-registry",
                        "ADAPTER_UNAVAILABLE",
                        &format!("{adapter_id}: {}", error.message),
                    )
                })?
                .snapshot
                .clone();
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

    fn aggregate_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.aggregate_observations(&self.adapter_observations())
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
