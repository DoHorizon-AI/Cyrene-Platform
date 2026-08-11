#![allow(deprecated)]

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use cy_kernel_api::{
    semantic::{self, Resource, ResourceState},
    DeviceBinding, EnforcementMode, HealthReport, HostInventoryProvider, InventorySnapshot,
    NodeCapabilities, ProviderError, ResourceProvider,
};
use cy_proto::{hardware_v1, semantic_v1};

use crate::{
    client::HardwareAdapter,
    convert::{ensure_inventory_fresh, resource_state_from_proto},
    credential::PeerCredentialExpectation,
    registry::UdsHardwareAdapterRegistry,
    transport::{read_frame, write_frame},
};

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
fn unknown_resource_state_is_rejected() {
    assert_eq!(
        resource_state_from_proto(semantic_v1::ResourceState::Unspecified as i32)
            .unwrap_err()
            .reason_code,
        "UNKNOWN_ENUM_VALUE"
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
