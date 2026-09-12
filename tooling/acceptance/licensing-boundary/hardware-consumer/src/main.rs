//! Compile-only external consumer implementing the public Adapter ports.

use cy_kernel_contract::{
    semantic, DeviceBinding, HealthReport, HostInventoryProvider, InventorySnapshot,
    NodeCapabilities, ProviderError, ResourceProvider,
};

struct ExternalAdapter;

impl HostInventoryProvider for ExternalAdapter {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            generation: 1,
            resources: Vec::new(),
            capabilities: NodeCapabilities {
                ready: true,
                facts: Vec::new(),
                enforcement: Vec::new(),
            },
        })
    }
}

impl ResourceProvider for ExternalAdapter {
    fn adapter_id(&self) -> &str {
        "external-adapter"
    }

    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError> {
        Ok(Vec::new())
    }

    fn create_binding(
        &self,
        _resource: &semantic::Resource,
    ) -> Result<DeviceBinding, ProviderError> {
        Err(ProviderError::new(
            self.adapter_id(),
            "NOT_IMPLEMENTED",
            "compile-only external consumer",
        ))
    }

    fn read_health(&self, _resource_id: &str) -> Result<HealthReport, ProviderError> {
        Ok(HealthReport {
            healthy: Some(true),
            reason_code: "COMPILE_ONLY".to_string(),
            summary: "external public adapter contract loaded".to_string(),
        })
    }
}

fn main() {
    let adapter = ExternalAdapter;
    assert_eq!(adapter.adapter_id(), "external-adapter");
}
