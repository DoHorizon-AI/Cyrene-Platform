// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/resource.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 资源、能力、提供商与查询模型。

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    identity::Identity,
    validation::{
        validate_capabilities, validate_namespaced_id, validate_properties, validate_text,
        ContractError, MAX_CAPABILITIES, MAX_ID_BYTES, MAX_PROPERTIES, MAX_RESOURCES_PER_LEASE,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub id: String,
    pub revision: u32,
    pub properties: BTreeMap<String, String>,
}

impl Capability {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("capability id", &self.id)?;
        if self.revision == 0 {
            return Err(ContractError::new(
                "CAPABILITY_REVISION_INVALID",
                "capability revision must be non-zero",
            ));
        }
        validate_properties(&self.properties)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequirement {
    pub id: String,
    pub minimum_revision: u32,
    pub required_properties: BTreeMap<String, String>,
}

impl CapabilityRequirement {
    pub fn matches(&self, capability: &Capability) -> bool {
        self.id == capability.id
            && capability.revision >= self.minimum_revision
            && self.required_properties.iter().all(|(key, value)| {
                capability
                    .properties
                    .get(key)
                    .is_some_and(|actual| actual == value)
            })
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("capability requirement id", &self.id)?;
        if self.minimum_revision == 0 {
            return Err(ContractError::new(
                "CAPABILITY_REVISION_INVALID",
                "minimum capability revision must be non-zero",
            ));
        }
        validate_properties(&self.required_properties)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantity {
    pub value: u64,
    pub unit: String,
}

impl Quantity {
    pub fn satisfies(&self, minimum: &Self) -> bool {
        self.unit == minimum.unit && self.value >= minimum.value
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("quantity unit", &self.unit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderState {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub identity: Identity,
    pub state: ProviderState,
    pub capabilities: Vec<Capability>,
}

impl Provider {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        validate_capabilities(&self.capabilities)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyLink {
    pub peer: Identity,
    pub kind: String,
    pub properties: BTreeMap<String, String>,
}

impl TopologyLink {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.peer.validate()?;
        validate_namespaced_id("topology kind", &self.kind)?;
        validate_properties(&self.properties)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub identity: Identity,
    pub provider: Identity,
    pub resource_class: String,
    pub capabilities: Vec<Capability>,
    pub capacity: BTreeMap<String, Quantity>,
    pub attributes: BTreeMap<String, String>,
    pub state: ResourceState,
    pub reason_code: String,
    pub summary: String,
    pub links: Vec<TopologyLink>,
}

impl Resource {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.provider.validate()?;
        validate_namespaced_id("resource class", &self.resource_class)?;
        validate_capabilities(&self.capabilities)?;
        if self.capacity.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "CAPACITY_LIMIT_EXCEEDED",
                "resource capacity has too many entries",
            ));
        }
        for (key, quantity) in &self.capacity {
            validate_namespaced_id("capacity key", key)?;
            quantity.validate()?;
        }
        validate_properties(&self.attributes)?;
        validate_namespaced_id("resource reason code", &self.reason_code)?;
        validate_text("resource summary", &self.summary, MAX_ID_BYTES)?;
        if self.links.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "TOPOLOGY_LIMIT_EXCEEDED",
                "resource has too many topology links",
            ));
        }
        let mut links = BTreeSet::new();
        for link in &self.links {
            link.validate()?;
            if link.peer == self.identity {
                return Err(ContractError::new(
                    "TOPOLOGY_SELF_LINK",
                    "resource topology cannot link to itself",
                ));
            }
            if !links.insert((link.peer.clone(), link.kind.as_str())) {
                return Err(ContractError::new(
                    "TOPOLOGY_LINK_DUPLICATE",
                    "resource contains a duplicate topology link",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceQuery {
    pub resource_class: String,
    pub count: u32,
    pub required_capabilities: Vec<CapabilityRequirement>,
    pub minimum_capacity: BTreeMap<String, Quantity>,
}

impl ResourceQuery {
    pub fn matches(&self, resource: &Resource) -> bool {
        self.resource_class == resource.resource_class
            && self.required_capabilities.iter().all(|requirement| {
                resource
                    .capabilities
                    .iter()
                    .any(|capability| requirement.matches(capability))
            })
            && self.minimum_capacity.iter().all(|(key, minimum)| {
                resource
                    .capacity
                    .get(key)
                    .is_some_and(|actual| actual.satisfies(minimum))
            })
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("resource class", &self.resource_class)?;
        if self.count == 0 {
            return Err(ContractError::new(
                "RESOURCE_COUNT_INVALID",
                "resource query count must be non-zero",
            ));
        }
        if self.count as usize > MAX_RESOURCES_PER_LEASE {
            return Err(ContractError::new(
                "RESOURCE_COUNT_LIMIT_EXCEEDED",
                "resource query count exceeds the per-lease limit",
            ));
        }
        if self.required_capabilities.len() > MAX_CAPABILITIES {
            return Err(ContractError::new(
                "CAPABILITY_LIMIT_EXCEEDED",
                "resource query has too many capability requirements",
            ));
        }
        let mut requirement_ids = BTreeSet::new();
        for requirement in &self.required_capabilities {
            requirement.validate()?;
            if !requirement_ids.insert(requirement.id.as_str()) {
                return Err(ContractError::new(
                    "CAPABILITY_REQUIREMENT_DUPLICATE",
                    "resource query contains a duplicate capability requirement",
                ));
            }
        }
        if self.minimum_capacity.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "CAPACITY_LIMIT_EXCEEDED",
                "resource query has too many capacity requirements",
            ));
        }
        for (key, quantity) in &self.minimum_capacity {
            validate_namespaced_id("capacity key", key)?;
            quantity.validate()?;
        }
        Ok(())
    }
}
