//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 capability.rs                                                   │
//! │  Module: cy_execution_fabric::capability                            │
//! │  Role: Canonical Capability property conventions for execution.     │
//! │                                                                     │
//! │  模块职责：定义执行底座在 canonical Capability 中使用的属性约定。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::HashMap;

use cy_proto::core_v1::{ExecutionAttachmentType, RestartCapability};
use cy_proto::semantic_v1::Capability;

use crate::FabricContractError;

pub const EXECUTION_CAPABILITY_ID: &str = "cyrene.execution.fabric.v1";
pub const ARTIFACT_TRANSFER_CAPABILITY_ID: &str = "cyrene.artifact.transfer.v1";

/// Typed encoder for execution facts stored in the canonical Capability map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCapabilityEnvelope {
    pub attachment: ExecutionAttachmentType,
    pub persistent: bool,
    pub restart: RestartCapability,
    pub host_supervision: bool,
    pub runtime_supervision: bool,
    pub provider_supervision: bool,
    pub checkpoint_resume: bool,
    pub persistent_volume: bool,
    pub persistent_cache: bool,
    pub outbound_https: bool,
    pub inbound_available: bool,
    pub peer_transfer: bool,
    pub cpu_millicores: Option<u64>,
    pub ram_bytes: Option<u64>,
    pub accelerator: Option<String>,
    pub accelerator_count: Option<u32>,
    pub vram_bytes: Option<u64>,
}

impl ExecutionCapabilityEnvelope {
    pub fn into_capability(self) -> Capability {
        let mut properties = HashMap::from([
            (
                "attachment_type".to_string(),
                self.attachment.as_str_name().to_string(),
            ),
            ("persistent".to_string(), self.persistent.to_string()),
            (
                "restart_capability".to_string(),
                self.restart.as_str_name().to_string(),
            ),
            (
                "host_supervision".to_string(),
                self.host_supervision.to_string(),
            ),
            (
                "runtime_supervision".to_string(),
                self.runtime_supervision.to_string(),
            ),
            (
                "provider_supervision".to_string(),
                self.provider_supervision.to_string(),
            ),
            (
                "checkpoint_resume".to_string(),
                self.checkpoint_resume.to_string(),
            ),
            (
                "persistent_volume".to_string(),
                self.persistent_volume.to_string(),
            ),
            (
                "persistent_cache".to_string(),
                self.persistent_cache.to_string(),
            ),
            (
                "outbound_https".to_string(),
                self.outbound_https.to_string(),
            ),
            (
                "inbound_available".to_string(),
                self.inbound_available.to_string(),
            ),
            ("peer_transfer".to_string(), self.peer_transfer.to_string()),
        ]);
        insert_optional(&mut properties, "cpu_millicores", self.cpu_millicores);
        insert_optional(&mut properties, "ram_bytes", self.ram_bytes);
        insert_optional(&mut properties, "accelerator_count", self.accelerator_count);
        insert_optional(&mut properties, "vram_bytes", self.vram_bytes);
        if let Some(accelerator) = self.accelerator {
            properties.insert("accelerator".to_string(), accelerator);
        }
        Capability {
            id: EXECUTION_CAPABILITY_ID.to_string(),
            revision: 1,
            properties,
        }
    }
}

/// Construct the execution envelope inside the existing semantic Capability.
///
/// The helper defines property names only; [`Capability`] remains the single
/// capability authority shared with Kernel matching and Provider snapshots.
pub fn execution_capability(
    attachment: ExecutionAttachmentType,
    persistent: bool,
    restart: RestartCapability,
) -> Capability {
    ExecutionCapabilityEnvelope {
        attachment,
        persistent,
        restart,
        host_supervision: restart == RestartCapability::HostSupervised,
        runtime_supervision: attachment != ExecutionAttachmentType::ProviderManaged,
        provider_supervision: restart == RestartCapability::ProviderSupervised,
        checkpoint_resume: false,
        persistent_volume: false,
        persistent_cache: false,
        outbound_https: true,
        inbound_available: false,
        peer_transfer: true,
        cpu_millicores: None,
        ram_bytes: None,
        accelerator: None,
        accelerator_count: None,
        vram_bytes: None,
    }
    .into_capability()
}

fn insert_optional<T: ToString>(
    properties: &mut HashMap<String, String>,
    key: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        properties.insert(key.to_string(), value.to_string());
    }
}

/// Construct resumable Peer-transfer facts in the canonical Capability shape.
pub fn artifact_transfer_capability() -> Capability {
    Capability {
        id: ARTIFACT_TRANSFER_CAPABILITY_ID.to_string(),
        revision: 1,
        properties: HashMap::from([
            ("https_range".to_string(), "true".to_string()),
            ("multipart".to_string(), "true".to_string()),
            ("resume".to_string(), "true".to_string()),
            ("peer_transfer".to_string(), "true".to_string()),
        ]),
    }
}

/// Verify that typed lifecycle fields and canonical Capability facts agree.
pub fn validate_execution_capability(
    capabilities: &[Capability],
    attachment: ExecutionAttachmentType,
    persistent: bool,
    restart: RestartCapability,
) -> Result<(), FabricContractError> {
    let capability = capabilities
        .iter()
        .find(|value| value.id == EXECUTION_CAPABILITY_ID && value.revision >= 1)
        .ok_or_else(|| {
            FabricContractError::new(
                "EXECUTION_CAPABILITY_REQUIRED",
                "canonical execution Capability is required",
            )
        })?;
    let expected = [
        ("attachment_type", attachment.as_str_name().to_string()),
        ("persistent", persistent.to_string()),
        ("restart_capability", restart.as_str_name().to_string()),
    ];
    if expected.iter().any(|(key, value)| {
        capability.properties.get(*key).map(String::as_str) != Some(value.as_str())
    }) {
        return Err(FabricContractError::new(
            "EXECUTION_CAPABILITY_MISMATCH",
            "typed execution lifecycle fields disagree with canonical Capability properties",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_capability_carries_attachment_persistence_and_restart() {
        let capability = execution_capability(
            ExecutionAttachmentType::ContainerAgent,
            false,
            RestartCapability::None,
        );
        assert_eq!(
            validate_execution_capability(
                &[capability],
                ExecutionAttachmentType::ContainerAgent,
                false,
                RestartCapability::None,
            ),
            Ok(())
        );
        let hardware = ExecutionCapabilityEnvelope {
            attachment: ExecutionAttachmentType::HostAgent,
            persistent: true,
            restart: RestartCapability::HostSupervised,
            host_supervision: true,
            runtime_supervision: true,
            provider_supervision: false,
            checkpoint_resume: true,
            persistent_volume: true,
            persistent_cache: true,
            outbound_https: true,
            inbound_available: false,
            peer_transfer: true,
            cpu_millicores: Some(16_000),
            ram_bytes: Some(64 * 1024 * 1024 * 1024),
            accelerator: Some("nvidia".to_string()),
            accelerator_count: Some(1),
            vram_bytes: Some(24 * 1024 * 1024 * 1024),
        }
        .into_capability();
        assert_eq!(
            hardware.properties.get("vram_bytes").map(String::as_str),
            Some("25769803776")
        );
    }
}
