//! Pure, transport-independent projection of CYRENE Kernel Semantic Contract v1.
//!
//! This crate contains no I/O, operating-system handles, serialization or
//! implementation-specific runtime types. Transport and language bindings map
//! to these semantics rather than making their own semantics.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

pub const CONTRACT_VERSION: &str = "cyrene.kernel.semantic/v1";
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_NAMESPACED_ID_BYTES: usize = 128;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_PROPERTIES: usize = 64;
pub const MAX_RESOURCES_PER_SNAPSHOT: usize = 1_024;
pub const MAX_EVENT_BODY_BYTES: usize = 64 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub reason_code: &'static str,
    pub message: String,
}

impl ContractError {
    fn new(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub id: String,
    pub generation: u64,
}

impl Identity {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text("identity id", &self.id, MAX_ID_BYTES)?;
        if self.generation == 0 {
            return Err(ContractError::new(
                "GENERATION_INVALID",
                "identity generation must be non-zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub identity: Identity,
}

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
        validate_text(
            "resource reason code",
            &self.reason_code,
            MAX_NAMESPACED_ID_BYTES,
        )?;
        validate_text("resource summary", &self.summary, MAX_ID_BYTES)?;
        if self.links.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "TOPOLOGY_LIMIT_EXCEEDED",
                "resource has too many topology links",
            ));
        }
        for link in &self.links {
            link.peer.validate()?;
            validate_namespaced_id("topology kind", &link.kind)?;
            validate_properties(&link.properties)?;
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
        if self.required_capabilities.len() > MAX_CAPABILITIES {
            return Err(ContractError::new(
                "CAPABILITY_LIMIT_EXCEEDED",
                "resource query has too many capability requirements",
            ));
        }
        for requirement in &self.required_capabilities {
            requirement.validate()?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Active,
    Releasing,
    Released,
    Expired,
    Revoked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub identity: Identity,
    pub holder: Identity,
    pub resources: Vec<Identity>,
    pub state: LeaseState,
    pub fence_token: u64,
    pub expires_at_unix_ms: Option<u64>,
}

impl Lease {
    pub fn authorizes(
        &self,
        holder: &Identity,
        resource: &Identity,
        fence_token: u64,
        now_unix_ms: u64,
    ) -> bool {
        self.state == LeaseState::Active
            && &self.holder == holder
            && self.resources.contains(resource)
            && self.fence_token == fence_token
            && self
                .expires_at_unix_ms
                .is_none_or(|expires_at| now_unix_ms < expires_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Registered,
    Starting,
    Running,
    Draining,
    Stopped,
    Failed,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub identity: Identity,
    pub principal: Identity,
    pub provider: Identity,
    pub lease: Identity,
    pub state: WorkerState,
    pub execution_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Created,
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub identity: Identity,
    pub owner: Identity,
    pub executor: Identity,
    pub kind: String,
    pub state: OperationState,
    pub deadline_unix_ms: Option<u64>,
    pub parent: Option<Identity>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub identity: Identity,
    pub provider: Identity,
    pub owner: Identity,
    pub transport: String,
    pub schema_id: String,
    pub capabilities: Vec<Capability>,
    pub public_attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointGrant {
    pub identity: Identity,
    pub endpoint: Identity,
    pub grantee: Identity,
    pub lease: Identity,
    pub fence_token: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub subject: Identity,
    pub kind: String,
    pub observed_at_unix_ms: u64,
    pub schema_id: String,
    pub body: Vec<u8>,
}

impl Event {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.sequence == 0 {
            return Err(ContractError::new(
                "EVENT_SEQUENCE_INVALID",
                "event sequence must be non-zero",
            ));
        }
        self.subject.validate()?;
        validate_namespaced_id("event kind", &self.kind)?;
        validate_namespaced_id("event schema id", &self.schema_id)?;
        if self.body.len() > MAX_EVENT_BODY_BYTES {
            return Err(ContractError::new(
                "EVENT_BODY_LIMIT_EXCEEDED",
                "event body exceeds the semantic contract limit",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub provider: Identity,
    pub snapshot_generation: u64,
    pub resources: Vec<Resource>,
    pub workers: Vec<Worker>,
    pub endpoints: Vec<Endpoint>,
    pub sampled_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

impl ProviderSnapshot {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.provider.validate()?;
        if self.snapshot_generation == 0 {
            return Err(ContractError::new(
                "SNAPSHOT_GENERATION_INVALID",
                "provider snapshot generation must be non-zero",
            ));
        }
        if self.expires_at_unix_ms <= self.sampled_at_unix_ms {
            return Err(ContractError::new(
                "SNAPSHOT_EXPIRY_INVALID",
                "provider snapshot expiry must be after sampling time",
            ));
        }
        if self.resources.len() > MAX_RESOURCES_PER_SNAPSHOT {
            return Err(ContractError::new(
                "RESOURCE_LIMIT_EXCEEDED",
                "provider snapshot has too many resources",
            ));
        }
        let mut identities = BTreeSet::new();
        for resource in &self.resources {
            resource.validate()?;
            if resource.provider != self.provider {
                return Err(ContractError::new(
                    "RESOURCE_PROVIDER_MISMATCH",
                    "resource provider does not match snapshot provider",
                ));
            }
            if !identities.insert(resource.identity.clone()) {
                return Err(ContractError::new(
                    "RESOURCE_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate resource identity",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticAction {
    Negotiate,
    RegisterProvider,
    PublishInventory,
    ReconcileProvider,
    AcquireLease,
    RenewLease,
    ReleaseLease,
    StartWorker,
    HeartbeatWorker,
    StopWorker,
    CreateOperation,
    ReportOperation,
    CancelOperation,
    PublishEndpoint,
    AuthorizeEndpoint,
    RevokeEndpoint,
    SubscribeEvents,
}

pub const V1_ACTIONS: &[SemanticAction] = &[
    SemanticAction::Negotiate,
    SemanticAction::RegisterProvider,
    SemanticAction::PublishInventory,
    SemanticAction::ReconcileProvider,
    SemanticAction::AcquireLease,
    SemanticAction::RenewLease,
    SemanticAction::ReleaseLease,
    SemanticAction::StartWorker,
    SemanticAction::HeartbeatWorker,
    SemanticAction::StopWorker,
    SemanticAction::CreateOperation,
    SemanticAction::ReportOperation,
    SemanticAction::CancelOperation,
    SemanticAction::PublishEndpoint,
    SemanticAction::AuthorizeEndpoint,
    SemanticAction::RevokeEndpoint,
    SemanticAction::SubscribeEvents,
];

fn validate_capabilities(capabilities: &[Capability]) -> Result<(), ContractError> {
    if capabilities.len() > MAX_CAPABILITIES {
        return Err(ContractError::new(
            "CAPABILITY_LIMIT_EXCEEDED",
            "object has too many capabilities",
        ));
    }
    let mut ids = BTreeSet::new();
    for capability in capabilities {
        capability.validate()?;
        if !ids.insert(capability.id.as_str()) {
            return Err(ContractError::new(
                "CAPABILITY_DUPLICATE",
                "object contains a duplicate capability id",
            ));
        }
    }
    Ok(())
}

fn validate_properties(properties: &BTreeMap<String, String>) -> Result<(), ContractError> {
    if properties.len() > MAX_PROPERTIES {
        return Err(ContractError::new(
            "PROPERTY_LIMIT_EXCEEDED",
            "object has too many properties",
        ));
    }
    for (key, value) in properties {
        validate_namespaced_id("property key", key)?;
        validate_text("property value", value, MAX_ID_BYTES)?;
    }
    Ok(())
}

fn validate_namespaced_id(field: &str, value: &str) -> Result<(), ContractError> {
    validate_text(field, value, MAX_NAMESPACED_ID_BYTES)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
    }) {
        return Err(ContractError::new(
            "NAMESPACED_ID_INVALID",
            format!("{field} contains an unsupported character"),
        ));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, max_bytes: usize) -> Result<(), ContractError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ContractError::new(
            "TEXT_INVALID",
            format!("{field} must be non-empty, bounded UTF-8 without control characters"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(id: &str, generation: u64) -> Identity {
        Identity {
            id: id.to_string(),
            generation,
        }
    }

    fn resource() -> Resource {
        Resource {
            identity: identity("resource-1", 1),
            provider: identity("provider-1", 3),
            resource_class: "accelerator".to_string(),
            capabilities: vec![Capability {
                id: "accelerator.compute".to_string(),
                revision: 2,
                properties: BTreeMap::from([("numeric".to_string(), "bf16".to_string())]),
            }],
            capacity: BTreeMap::from([(
                "memory".to_string(),
                Quantity {
                    value: 80,
                    unit: "gib".to_string(),
                },
            )]),
            attributes: BTreeMap::new(),
            state: ResourceState::Ready,
            reason_code: "ready".to_string(),
            summary: "resource is ready".to_string(),
            links: Vec::new(),
        }
    }

    #[test]
    fn generic_query_matches_without_vendor_knowledge() {
        let query = ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::from([("numeric".to_string(), "bf16".to_string())]),
            }],
            minimum_capacity: BTreeMap::from([(
                "memory".to_string(),
                Quantity {
                    value: 40,
                    unit: "gib".to_string(),
                },
            )]),
        };
        query.validate().unwrap();
        assert!(query.matches(&resource()));
    }

    #[test]
    fn quantity_units_and_capability_properties_are_not_coerced() {
        let mut wrong_unit = ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: Vec::new(),
            minimum_capacity: BTreeMap::from([(
                "memory".to_string(),
                Quantity {
                    value: 40,
                    unit: "gb".to_string(),
                },
            )]),
        };
        assert!(!wrong_unit.matches(&resource()));
        wrong_unit.minimum_capacity.clear();
        wrong_unit
            .required_capabilities
            .push(CapabilityRequirement {
                id: "accelerator.compute".to_string(),
                minimum_revision: 1,
                required_properties: BTreeMap::from([("numeric".to_string(), "fp8".to_string())]),
            });
        assert!(!wrong_unit.matches(&resource()));
    }

    #[test]
    fn lease_authority_requires_identity_generation_fence_and_expiry() {
        let holder = identity("worker-1", 2);
        let resource = identity("resource-1", 5);
        let lease = Lease {
            identity: identity("lease-1", 1),
            holder: holder.clone(),
            resources: vec![resource.clone()],
            state: LeaseState::Active,
            fence_token: 42,
            expires_at_unix_ms: Some(1_000),
        };
        assert!(lease.authorizes(&holder, &resource, 42, 999));
        assert!(!lease.authorizes(&identity("worker-1", 1), &resource, 42, 999));
        assert!(!lease.authorizes(&holder, &resource, 41, 999));
        assert!(!lease.authorizes(&holder, &resource, 42, 1_000));
    }

    #[test]
    fn snapshots_reject_unowned_or_duplicate_resources() {
        let provider = identity("provider-1", 3);
        let mut snapshot = ProviderSnapshot {
            provider: provider.clone(),
            snapshot_generation: 1,
            resources: vec![resource()],
            workers: Vec::new(),
            endpoints: Vec::new(),
            sampled_at_unix_ms: 10,
            expires_at_unix_ms: 20,
        };
        snapshot.validate().unwrap();
        snapshot.resources.push(resource());
        assert_eq!(
            snapshot.validate().unwrap_err().reason_code,
            "RESOURCE_IDENTITY_DUPLICATE"
        );
    }

    #[test]
    fn event_body_is_bounded() {
        let event = Event {
            sequence: 1,
            subject: identity("worker-1", 1),
            kind: "worker.started".to_string(),
            observed_at_unix_ms: 1,
            schema_id: "cyrene.event.worker-started.v1".to_string(),
            body: vec![0; MAX_EVENT_BODY_BYTES + 1],
        };
        assert_eq!(
            event.validate().unwrap_err().reason_code,
            "EVENT_BODY_LIMIT_EXCEEDED"
        );
    }
}
