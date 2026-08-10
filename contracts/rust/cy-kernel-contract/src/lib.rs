//! Pure, transport-independent projection of CYRENE Kernel Semantic Contract v1.
//!
//! This crate contains no I/O, operating-system handles, serialization or
//! implementation-specific runtime types. Transport and language bindings map
//! to these semantics rather than making their own semantics.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

pub const CONTRACT_ID: &str = "cyrene.kernel.semantic";
pub const CONTRACT_VERSION: &str = "cyrene.kernel.semantic/v1";
pub const CONTRACT_MAJOR: u32 = 1;
pub const CONTRACT_MINOR: u32 = 0;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_NAMESPACED_ID_BYTES: usize = 128;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_PROPERTIES: usize = 64;
pub const MAX_RESOURCES_PER_SNAPSHOT: usize = 1_024;
pub const MAX_RESOURCES_PER_LEASE: usize = 256;
pub const MAX_WORKERS_PER_SNAPSHOT: usize = 4_096;
pub const MAX_ENDPOINTS_PER_SNAPSHOT: usize = 4_096;
pub const MAX_EXECUTION_REF_BYTES: usize = 512;
pub const MAX_ERROR_MESSAGE_BYTES: usize = 1_024;
pub const MAX_EVENT_BODY_BYTES: usize = 64 * 1_024;
pub const MAX_EVENTS_PER_PAGE: usize = 256;
pub const MAX_TIMESTAMP_UNIX_MS: u64 = 253_402_300_799_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRevision {
    pub contract_id: String,
    pub major: u32,
    pub minor: u32,
}

impl ContractRevision {
    pub fn current() -> Self {
        Self {
            contract_id: CONTRACT_ID.to_string(),
            major: CONTRACT_MAJOR,
            minor: CONTRACT_MINOR,
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_namespaced_id("contract id", &self.contract_id)?;
        if self.major == 0 {
            return Err(ContractError::new(
                "CONTRACT_MAJOR_INVALID",
                "semantic contract major version must be non-zero",
            ));
        }
        Ok(())
    }

    /// Compatible revisions have the same contract ID and major version. The
    /// negotiated minor is the lower supported minor.
    pub fn negotiate(&self, offered: &Self) -> Option<Self> {
        if self.validate().is_err()
            || offered.validate().is_err()
            || self.contract_id != offered.contract_id
            || self.major != offered.major
        {
            return None;
        }
        Some(Self {
            contract_id: self.contract_id.clone(),
            major: self.major,
            minor: self.minor.min(offered.minor),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub reason_code: String,
    pub message: String,
}

impl Rejection {
    pub fn new(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason_code: reason_code.into(),
            message: message.into(),
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_reason_code(&self.reason_code)?;
        validate_text("rejection message", &self.message, MAX_ERROR_MESSAGE_BYTES)
    }
}

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

impl Principal {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Active,
    Releasing,
    Released,
    Expired,
    Revoked,
    Failed,
}

impl LeaseState {
    /// Whether a state report is an idempotent replay or a legal forward-only
    /// transition for the same Lease generation.
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Active,
                    Self::Releasing | Self::Expired | Self::Revoked | Self::Failed
                ) | (
                    Self::Releasing,
                    Self::Released | Self::Revoked | Self::Failed
                )
            )
    }
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
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.holder.validate()?;
        if self.resources.is_empty() || self.resources.len() > MAX_RESOURCES_PER_LEASE {
            return Err(ContractError::new(
                "LEASE_RESOURCE_LIMIT_INVALID",
                "lease must contain a bounded, non-empty resource set",
            ));
        }
        let mut resources = BTreeSet::new();
        for resource in &self.resources {
            resource.validate()?;
            if !resources.insert(resource) {
                return Err(ContractError::new(
                    "LEASE_RESOURCE_DUPLICATE",
                    "lease contains a duplicate resource identity",
                ));
            }
        }
        if self.fence_token == 0 {
            return Err(ContractError::new(
                "FENCE_TOKEN_INVALID",
                "lease fence token must be non-zero",
            ));
        }
        let expires_at = self.expires_at_unix_ms.ok_or_else(|| {
            ContractError::new(
                "LEASE_EXPIRY_REQUIRED",
                "v1 leases must have a finite expiry",
            )
        })?;
        validate_timestamp("lease expiry", expires_at)
    }

    pub fn authorizes(
        &self,
        holder: &Identity,
        resource: &Identity,
        fence_token: u64,
        now_unix_ms: u64,
    ) -> bool {
        self.validate().is_ok()
            && self.state == LeaseState::Active
            && &self.holder == holder
            && self.resources.contains(resource)
            && self.fence_token == fence_token
            && self
                .expires_at_unix_ms
                .is_some_and(|expires_at| now_unix_ms < expires_at)
    }

    pub fn renew(
        &self,
        fence_token: u64,
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<Self, ContractError> {
        self.validate()?;
        if self.state != LeaseState::Active {
            return Err(ContractError::new(
                "LEASE_NOT_ACTIVE",
                "only an active lease can be renewed",
            ));
        }
        let current_expiry = self.expires_at_unix_ms.ok_or_else(|| {
            ContractError::new(
                "LEASE_EXPIRY_REQUIRED",
                "v1 leases must have a finite expiry",
            )
        })?;
        if now_unix_ms >= current_expiry {
            return Err(ContractError::new(
                "LEASE_EXPIRED",
                "an expired lease cannot be renewed",
            ));
        }
        if fence_token != self.fence_token {
            return Err(ContractError::new(
                "FENCE_MISMATCH",
                "lease renewal fence does not match current authority",
            ));
        }
        validate_timestamp("lease renewal expiry", expires_at_unix_ms)?;
        if expires_at_unix_ms <= current_expiry {
            return Err(ContractError::new(
                "LEASE_RENEWAL_INVALID",
                "lease renewal must strictly extend the finite expiry",
            ));
        }
        let mut renewed = self.clone();
        renewed.expires_at_unix_ms = Some(expires_at_unix_ms);
        Ok(renewed)
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

impl WorkerState {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Registered,
                    Self::Starting | Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (
                    Self::Starting,
                    Self::Running | Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (
                    Self::Running,
                    Self::Draining | Self::Stopped | Self::Failed | Self::Lost
                ) | (Self::Draining, Self::Stopped | Self::Failed | Self::Lost)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub identity: Identity,
    pub principal: Identity,
    pub provider: Identity,
    pub lease: Identity,
    pub state: WorkerState,
    pub execution_ref: String,
    pub limits: BTreeMap<String, Quantity>,
}

impl Worker {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.principal.validate()?;
        self.provider.validate()?;
        self.lease.validate()?;
        validate_text(
            "worker execution reference",
            &self.execution_ref,
            MAX_EXECUTION_REF_BYTES,
        )?;
        if self.limits.len() > MAX_PROPERTIES {
            return Err(ContractError::new(
                "WORKER_LIMIT_COUNT_EXCEEDED",
                "worker has too many generic execution limits",
            ));
        }
        for (key, quantity) in &self.limits {
            validate_namespaced_id("worker limit key", key)?;
            quantity.validate()?;
        }
        Ok(())
    }
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

impl OperationState {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Created,
                    Self::Pending
                        | Self::Running
                        | Self::Cancelling
                        | Self::Cancelled
                        | Self::Failed
                ) | (
                    Self::Pending,
                    Self::Running | Self::Cancelling | Self::Cancelled | Self::Failed | Self::Lost
                ) | (
                    Self::Running,
                    Self::Succeeded | Self::Failed | Self::Cancelling | Self::Lost
                ) | (
                    Self::Cancelling,
                    Self::Cancelled | Self::Failed | Self::Lost
                )
            )
    }
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

impl Operation {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.owner.validate()?;
        self.executor.validate()?;
        validate_namespaced_id("operation kind", &self.kind)?;
        if let Some(deadline) = self.deadline_unix_ms {
            validate_timestamp("operation deadline", deadline)?;
        }
        if let Some(parent) = &self.parent {
            parent.validate()?;
            if parent == &self.identity {
                return Err(ContractError::new(
                    "OPERATION_PARENT_INVALID",
                    "operation cannot be its own parent",
                ));
            }
        }
        validate_properties(&self.metadata)
    }
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

impl Endpoint {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.provider.validate()?;
        self.owner.validate()?;
        validate_namespaced_id("endpoint transport", &self.transport)?;
        validate_namespaced_id("endpoint schema id", &self.schema_id)?;
        validate_capabilities(&self.capabilities)?;
        validate_properties(&self.public_attributes)
    }
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

impl EndpointGrant {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.endpoint.validate()?;
        self.grantee.validate()?;
        self.lease.validate()?;
        if self.fence_token == 0 {
            return Err(ContractError::new(
                "FENCE_TOKEN_INVALID",
                "endpoint grant fence token must be non-zero",
            ));
        }
        validate_timestamp("endpoint grant expiry", self.expires_at_unix_ms)
    }

    /// Endpoint data bypasses Kernel, but the grant remains authorized only
    /// while both the grant and its referenced Lease authority are current.
    pub fn authorizes(
        &self,
        endpoint: &Identity,
        grantee: &Identity,
        lease: &Lease,
        now_unix_ms: u64,
    ) -> bool {
        self.validate().is_ok()
            && lease.validate().is_ok()
            && &self.endpoint == endpoint
            && &self.grantee == grantee
            && self.lease == lease.identity
            && self.grantee == lease.holder
            && self.fence_token == lease.fence_token
            && lease.state == LeaseState::Active
            && lease
                .expires_at_unix_ms
                .is_some_and(|expires_at| now_unix_ms < expires_at)
            && now_unix_ms < self.expires_at_unix_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub source: Identity,
    pub subject: Identity,
    pub kind: String,
    pub observed_at_unix_ms: u64,
    pub schema_id: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCursor {
    pub source: Identity,
    pub sequence: u64,
}

impl EventCursor {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.source.validate()
    }

    pub fn status_against(
        &self,
        current_source: &Identity,
        oldest_available_sequence: u64,
    ) -> ReplayStatus {
        if &self.source != current_source {
            ReplayStatus::SourceChanged
        } else if oldest_available_sequence > 0
            && self.sequence.saturating_add(1) < oldest_available_sequence
        {
            ReplayStatus::Gap
        } else {
            ReplayStatus::Current
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayStatus {
    Current,
    Gap,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub source: Identity,
    pub status: ReplayStatus,
    pub events: Vec<Event>,
    pub oldest_available_sequence: u64,
    pub latest_available_sequence: u64,
    pub next_sequence: u64,
}

impl EventPage {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.source.validate()?;
        if self.events.len() > MAX_EVENTS_PER_PAGE {
            return Err(ContractError::new(
                "EVENT_PAGE_LIMIT_EXCEEDED",
                "event page exceeds the semantic contract limit",
            ));
        }
        if self.oldest_available_sequence > self.latest_available_sequence {
            return Err(ContractError::new(
                "EVENT_RANGE_INVALID",
                "event replay range is inverted",
            ));
        }
        if (self.oldest_available_sequence == 0) != (self.latest_available_sequence == 0) {
            return Err(ContractError::new(
                "EVENT_RANGE_INVALID",
                "event replay range must be wholly empty or wholly non-zero",
            ));
        }
        if self.status != ReplayStatus::Current && !self.events.is_empty() {
            return Err(ContractError::new(
                "EVENT_RECONCILE_REQUIRED",
                "gap and source-change pages cannot contain incremental events",
            ));
        }
        let mut previous = None;
        for event in &self.events {
            event.validate()?;
            if event.source != self.source
                || previous.is_some_and(|sequence| event.sequence <= sequence)
                || event.sequence < self.oldest_available_sequence
                || event.sequence > self.latest_available_sequence
            {
                return Err(ContractError::new(
                    "EVENT_ORDER_INVALID",
                    "event page sources must match and sequences must strictly increase",
                ));
            }
            previous = Some(event.sequence);
        }
        if let Some(last) = previous {
            if self.next_sequence != last {
                return Err(ContractError::new(
                    "EVENT_CURSOR_INVALID",
                    "event page cursor must equal the last returned sequence",
                ));
            }
        }
        Ok(())
    }
}

impl Event {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.sequence == 0 {
            return Err(ContractError::new(
                "EVENT_SEQUENCE_INVALID",
                "event sequence must be non-zero",
            ));
        }
        self.source.validate()?;
        self.subject.validate()?;
        validate_namespaced_id("event kind", &self.kind)?;
        validate_timestamp("event observed time", self.observed_at_unix_ms)?;
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
        validate_timestamp("provider snapshot sample time", self.sampled_at_unix_ms)?;
        validate_timestamp("provider snapshot expiry", self.expires_at_unix_ms)?;
        if self.resources.len() > MAX_RESOURCES_PER_SNAPSHOT {
            return Err(ContractError::new(
                "RESOURCE_LIMIT_EXCEEDED",
                "provider snapshot has too many resources",
            ));
        }
        let mut resource_ids = BTreeSet::new();
        for resource in &self.resources {
            resource.validate()?;
            if resource.provider != self.provider {
                return Err(ContractError::new(
                    "RESOURCE_PROVIDER_MISMATCH",
                    "resource provider does not match snapshot provider",
                ));
            }
            if !resource_ids.insert(resource.identity.id.as_str()) {
                return Err(ContractError::new(
                    "RESOURCE_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate resource identity",
                ));
            }
        }
        if self.workers.len() > MAX_WORKERS_PER_SNAPSHOT {
            return Err(ContractError::new(
                "WORKER_LIMIT_EXCEEDED",
                "provider snapshot has too many workers",
            ));
        }
        let mut worker_ids = BTreeSet::new();
        for worker in &self.workers {
            worker.validate()?;
            if worker.provider != self.provider {
                return Err(ContractError::new(
                    "WORKER_PROVIDER_MISMATCH",
                    "worker provider does not match snapshot provider",
                ));
            }
            if !worker_ids.insert(worker.identity.id.as_str()) {
                return Err(ContractError::new(
                    "WORKER_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate worker identity",
                ));
            }
        }
        if self.endpoints.len() > MAX_ENDPOINTS_PER_SNAPSHOT {
            return Err(ContractError::new(
                "ENDPOINT_LIMIT_EXCEEDED",
                "provider snapshot has too many endpoints",
            ));
        }
        let mut endpoint_ids = BTreeSet::new();
        for endpoint in &self.endpoints {
            endpoint.validate()?;
            if endpoint.provider != self.provider {
                return Err(ContractError::new(
                    "ENDPOINT_PROVIDER_MISMATCH",
                    "endpoint provider does not match snapshot provider",
                ));
            }
            if !endpoint_ids.insert(endpoint.identity.id.as_str()) {
                return Err(ContractError::new(
                    "ENDPOINT_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate endpoint identity",
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
    let mut expect_segment_start = true;
    for byte in value.bytes() {
        if matches!(byte, b'.' | b'-' | b'_') {
            if expect_segment_start {
                return Err(ContractError::new(
                    "NAMESPACED_ID_INVALID",
                    format!("{field} contains an empty identifier segment"),
                ));
            }
            expect_segment_start = true;
        } else if expect_segment_start {
            if !byte.is_ascii_lowercase() {
                return Err(ContractError::new(
                    "NAMESPACED_ID_INVALID",
                    format!("{field} segments must start with a lower-case letter"),
                ));
            }
            expect_segment_start = false;
        } else if !(byte.is_ascii_lowercase() || byte.is_ascii_digit()) {
            return Err(ContractError::new(
                "NAMESPACED_ID_INVALID",
                format!("{field} contains an unsupported character"),
            ));
        }
    }
    if expect_segment_start {
        return Err(ContractError::new(
            "NAMESPACED_ID_INVALID",
            format!("{field} cannot end with a separator"),
        ));
    }
    Ok(())
}

fn validate_timestamp(field: &str, unix_ms: u64) -> Result<(), ContractError> {
    if unix_ms == 0 || unix_ms > MAX_TIMESTAMP_UNIX_MS {
        return Err(ContractError::new(
            "TIMESTAMP_INVALID",
            format!("{field} must be a positive Protobuf-compatible Unix millisecond value"),
        ));
    }
    Ok(())
}

fn validate_reason_code(value: &str) -> Result<(), ContractError> {
    validate_text("reason code", value, MAX_NAMESPACED_ID_BYTES)?;
    if !value.bytes().enumerate().all(|(index, byte)| {
        if index == 0 {
            byte.is_ascii_uppercase()
        } else {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
        }
    }) {
        return Err(ContractError::new(
            "REASON_CODE_INVALID",
            "reason code must use upper snake case",
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

    fn worker(provider: &Identity) -> Worker {
        Worker {
            identity: identity("worker-1", 1),
            principal: identity("principal-1", 1),
            provider: provider.clone(),
            lease: identity("lease-1", 1),
            state: WorkerState::Running,
            execution_ref: "artifact.sha256.0123456789abcdef".to_string(),
            limits: BTreeMap::new(),
        }
    }

    fn endpoint(provider: &Identity) -> Endpoint {
        Endpoint {
            identity: identity("endpoint-1", 1),
            provider: provider.clone(),
            owner: identity("worker-1", 1),
            transport: "local.uds".to_string(),
            schema_id: "cyrene.endpoint.echo.v1".to_string(),
            capabilities: Vec::new(),
            public_attributes: BTreeMap::new(),
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
    fn query_rejects_duplicate_requirements_and_unbounded_counts() {
        let requirement = CapabilityRequirement {
            id: "accelerator.compute".to_string(),
            minimum_revision: 1,
            required_properties: BTreeMap::new(),
        };
        let mut query = ResourceQuery {
            resource_class: "accelerator".to_string(),
            count: 1,
            required_capabilities: vec![requirement.clone(), requirement],
            minimum_capacity: BTreeMap::new(),
        };
        assert_eq!(
            query.validate().unwrap_err().reason_code,
            "CAPABILITY_REQUIREMENT_DUPLICATE"
        );
        query.required_capabilities.clear();
        query.count = MAX_RESOURCES_PER_LEASE as u32 + 1;
        assert_eq!(
            query.validate().unwrap_err().reason_code,
            "RESOURCE_COUNT_LIMIT_EXCEEDED"
        );
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

        let mut unbounded = lease;
        unbounded.expires_at_unix_ms = None;
        assert_eq!(
            unbounded.validate().unwrap_err().reason_code,
            "LEASE_EXPIRY_REQUIRED"
        );
        assert!(!unbounded.authorizes(&holder, &resource, 42, 999));
    }

    #[test]
    fn endpoint_grant_requires_both_grant_and_lease_authority() {
        let lease = Lease {
            identity: identity("lease-1", 2),
            holder: identity("worker-client", 4),
            resources: vec![identity("resource-1", 3)],
            state: LeaseState::Active,
            fence_token: 9,
            expires_at_unix_ms: Some(2_000),
        };
        let grant = EndpointGrant {
            identity: identity("grant-1", 1),
            endpoint: identity("endpoint-1", 1),
            grantee: identity("worker-client", 4),
            lease: lease.identity.clone(),
            fence_token: lease.fence_token,
            expires_at_unix_ms: 1_500,
        };
        assert!(grant.authorizes(&grant.endpoint, &grant.grantee, &lease, 1_000));
        assert!(!grant.authorizes(
            &grant.endpoint,
            &identity("worker-client", 3),
            &lease,
            1_000
        ));
        assert!(!grant.authorizes(&grant.endpoint, &grant.grantee, &lease, 1_500));
    }

    #[test]
    fn lifecycle_transitions_are_forward_only_and_terminal() {
        assert!(LeaseState::Active.can_transition_to(LeaseState::Releasing));
        assert!(LeaseState::Releasing.can_transition_to(LeaseState::Released));
        assert!(!LeaseState::Released.can_transition_to(LeaseState::Active));

        assert!(WorkerState::Registered.can_transition_to(WorkerState::Starting));
        assert!(WorkerState::Running.can_transition_to(WorkerState::Draining));
        assert!(!WorkerState::Stopped.can_transition_to(WorkerState::Running));

        assert!(OperationState::Created.can_transition_to(OperationState::Pending));
        assert!(OperationState::Running.can_transition_to(OperationState::Succeeded));
        assert!(!OperationState::Succeeded.can_transition_to(OperationState::Running));
    }

    #[test]
    fn namespaced_identifiers_and_timestamps_have_one_frozen_grammar() {
        for value in ["Vendor.cuda", "vendor..cuda", ".vendor", "vendor-"] {
            assert_eq!(
                validate_namespaced_id("test", value)
                    .unwrap_err()
                    .reason_code,
                "NAMESPACED_ID_INVALID"
            );
        }
        for value in ["vendor.nvidia.cuda", "local.uds", "byte", "schema-v1"] {
            validate_namespaced_id("test", value).unwrap();
        }
        assert_eq!(
            validate_timestamp("test", MAX_TIMESTAMP_UNIX_MS + 1)
                .unwrap_err()
                .reason_code,
            "TIMESTAMP_INVALID"
        );
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
        let mut duplicate_incarnation = resource();
        duplicate_incarnation.identity.generation += 1;
        snapshot.resources.push(duplicate_incarnation);
        assert_eq!(
            snapshot.validate().unwrap_err().reason_code,
            "RESOURCE_IDENTITY_DUPLICATE"
        );

        snapshot.resources.pop();
        snapshot
            .workers
            .push(worker(&identity("provider-other", 1)));
        assert_eq!(
            snapshot.validate().unwrap_err().reason_code,
            "WORKER_PROVIDER_MISMATCH"
        );
        snapshot.workers.clear();
        snapshot
            .endpoints
            .push(endpoint(&identity("provider-other", 1)));
        assert_eq!(
            snapshot.validate().unwrap_err().reason_code,
            "ENDPOINT_PROVIDER_MISMATCH"
        );
    }

    #[test]
    fn event_body_is_bounded() {
        let event = Event {
            sequence: 1,
            source: identity("node-1", 7),
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

    #[test]
    fn event_pages_are_source_scoped_ordered_and_gap_explicit() {
        let source = identity("node-1", 7);
        let event = Event {
            sequence: 5,
            source: source.clone(),
            subject: identity("worker-1", 1),
            kind: "worker.running".to_string(),
            observed_at_unix_ms: 10,
            schema_id: "cyrene.event.worker-running.v1".to_string(),
            body: Vec::new(),
        };
        let page = EventPage {
            source,
            status: ReplayStatus::Current,
            events: vec![event],
            oldest_available_sequence: 3,
            latest_available_sequence: 5,
            next_sequence: 5,
        };
        page.validate().unwrap();

        let mut gap_with_data = page;
        gap_with_data.status = ReplayStatus::Gap;
        assert_eq!(
            gap_with_data.validate().unwrap_err().reason_code,
            "EVENT_RECONCILE_REQUIRED"
        );
    }

    fn fixture_rows(input: &str) -> impl Iterator<Item = Vec<&str>> {
        input
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split('|').collect())
    }

    fn lease_state(value: &str) -> LeaseState {
        match value {
            "ACTIVE" => LeaseState::Active,
            "RELEASING" => LeaseState::Releasing,
            "RELEASED" => LeaseState::Released,
            "EXPIRED" => LeaseState::Expired,
            "REVOKED" => LeaseState::Revoked,
            "FAILED" => LeaseState::Failed,
            _ => panic!("unknown Lease state: {value}"),
        }
    }

    fn worker_state(value: &str) -> WorkerState {
        match value {
            "REGISTERED" => WorkerState::Registered,
            "STARTING" => WorkerState::Starting,
            "RUNNING" => WorkerState::Running,
            "DRAINING" => WorkerState::Draining,
            "STOPPED" => WorkerState::Stopped,
            "FAILED" => WorkerState::Failed,
            "LOST" => WorkerState::Lost,
            _ => panic!("unknown Worker state: {value}"),
        }
    }

    fn operation_state(value: &str) -> OperationState {
        match value {
            "CREATED" => OperationState::Created,
            "PENDING" => OperationState::Pending,
            "RUNNING" => OperationState::Running,
            "SUCCEEDED" => OperationState::Succeeded,
            "FAILED" => OperationState::Failed,
            "CANCELLING" => OperationState::Cancelling,
            "CANCELLED" => OperationState::Cancelled,
            "LOST" => OperationState::Lost,
            _ => panic!("unknown Operation state: {value}"),
        }
    }

    fn fixture_properties(value: &str) -> BTreeMap<String, String> {
        if value == "-" {
            return BTreeMap::new();
        }
        value
            .split(',')
            .map(|item| {
                let (key, value) = item.split_once('=').unwrap();
                (key.to_string(), value.to_string())
            })
            .collect()
    }

    fn fixture_capacity(value: &str) -> BTreeMap<String, Quantity> {
        if value == "-" {
            return BTreeMap::new();
        }
        value
            .split(',')
            .map(|item| {
                let (key, quantity) = item.split_once('=').unwrap();
                let (value, unit) = quantity.split_once('@').unwrap();
                (
                    key.to_string(),
                    Quantity {
                        value: value.parse().unwrap(),
                        unit: unit.to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn frozen_tck_limits_identifiers_and_negotiation_match_rust() {
        let expected_limits = BTreeMap::from([
            ("max_id_bytes", MAX_ID_BYTES as u64),
            ("max_namespaced_id_bytes", MAX_NAMESPACED_ID_BYTES as u64),
            ("max_capabilities", MAX_CAPABILITIES as u64),
            ("max_properties", MAX_PROPERTIES as u64),
            (
                "max_resources_per_snapshot",
                MAX_RESOURCES_PER_SNAPSHOT as u64,
            ),
            ("max_resources_per_lease", MAX_RESOURCES_PER_LEASE as u64),
            ("max_workers_per_snapshot", MAX_WORKERS_PER_SNAPSHOT as u64),
            (
                "max_endpoints_per_snapshot",
                MAX_ENDPOINTS_PER_SNAPSHOT as u64,
            ),
            ("max_execution_ref_bytes", MAX_EXECUTION_REF_BYTES as u64),
            ("max_error_message_bytes", MAX_ERROR_MESSAGE_BYTES as u64),
            ("max_event_body_bytes", MAX_EVENT_BODY_BYTES as u64),
            ("max_events_per_page", MAX_EVENTS_PER_PAGE as u64),
            ("max_timestamp_unix_ms", MAX_TIMESTAMP_UNIX_MS),
        ]);
        let actual_limits =
            fixture_rows(include_str!("../../../tck/kernel-semantic/v1/limits.tsv"))
                .map(|row| (row[0], row[1].parse::<u64>().unwrap()))
                .collect::<BTreeMap<_, _>>();
        assert_eq!(actual_limits, expected_limits);

        for row in fixture_rows(include_str!(
            "../../../tck/kernel-semantic/v1/identifiers.tsv"
        )) {
            let actual = match row[1] {
                "namespaced" => validate_namespaced_id("fixture", row[2]),
                "identity" => Identity {
                    id: match row[2] {
                        "<empty>" => "",
                        "<c0>" => "\u{0001}",
                        "<c1>" => "\u{0085}",
                        value => value,
                    }
                    .to_string(),
                    generation: 1,
                }
                .validate(),
                "timestamp" => validate_timestamp("fixture", row[2].parse().unwrap()),
                kind => panic!("unknown identifier fixture kind: {kind}"),
            };
            let actual = actual
                .map(|()| "ACCEPT")
                .unwrap_or_else(|error| error.reason_code);
            assert_eq!(actual, row[3], "{}", row[0]);
        }

        for row in fixture_rows(include_str!(
            "../../../tck/kernel-semantic/v1/negotiation.tsv"
        )) {
            let local = ContractRevision {
                contract_id: row[1].to_string(),
                major: row[2].parse().unwrap(),
                minor: row[3].parse().unwrap(),
            };
            let offered = ContractRevision {
                contract_id: row[4].to_string(),
                major: row[5].parse().unwrap(),
                minor: row[6].parse().unwrap(),
            };
            let actual = local.negotiate(&offered).map_or_else(
                || "INCOMPATIBLE".to_string(),
                |value| format!("{}.{}", value.major, value.minor),
            );
            assert_eq!(actual, row[7], "{}", row[0]);
        }
    }

    #[test]
    fn frozen_tck_state_matrices_are_complete() {
        for row in fixture_rows(include_str!(
            "../../../tck/kernel-semantic/v1/transitions.tsv"
        )) {
            let allowed = row[2].split(',').collect::<BTreeSet<_>>();
            let all_states: &[&str] = match row[0] {
                "lease" => &[
                    "ACTIVE",
                    "RELEASING",
                    "RELEASED",
                    "EXPIRED",
                    "REVOKED",
                    "FAILED",
                ],
                "worker" => &[
                    "REGISTERED",
                    "STARTING",
                    "RUNNING",
                    "DRAINING",
                    "STOPPED",
                    "FAILED",
                    "LOST",
                ],
                "operation" => &[
                    "CREATED",
                    "PENDING",
                    "RUNNING",
                    "SUCCEEDED",
                    "FAILED",
                    "CANCELLING",
                    "CANCELLED",
                    "LOST",
                ],
                noun => panic!("unknown lifecycle noun: {noun}"),
            };
            for target in all_states {
                let actual = match row[0] {
                    "lease" => lease_state(row[1]).can_transition_to(lease_state(target)),
                    "worker" => worker_state(row[1]).can_transition_to(worker_state(target)),
                    "operation" => {
                        operation_state(row[1]).can_transition_to(operation_state(target))
                    }
                    _ => unreachable!(),
                };
                assert_eq!(
                    actual,
                    allowed.contains(target),
                    "{}.{} -> {target}",
                    row[0],
                    row[1]
                );
            }
        }
    }

    #[test]
    fn frozen_tck_matching_authority_and_replay_match_rust() {
        for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/matching.tsv")) {
            let capability = Capability {
                id: row[1].to_string(),
                revision: row[2].parse().unwrap(),
                properties: fixture_properties(row[3]),
            };
            let requirement = CapabilityRequirement {
                id: row[4].to_string(),
                minimum_revision: row[5].parse().unwrap(),
                required_properties: fixture_properties(row[6]),
            };
            let capacity = fixture_capacity(row[7]);
            let minimum = fixture_capacity(row[8]);
            let actual = requirement.matches(&capability)
                && minimum.iter().all(|(key, minimum)| {
                    capacity
                        .get(key)
                        .is_some_and(|actual| actual.satisfies(minimum))
                });
            assert_eq!(actual, row[9] == "true", "{}", row[0]);
        }

        for row in fixture_rows(include_str!(
            "../../../tck/kernel-semantic/v1/authority.tsv"
        )) {
            let state = lease_state(row[2]);
            let holder = identity("worker-holder", 2);
            let presented_holder = if row[3] == "true" {
                holder.clone()
            } else {
                identity("worker-holder", 1)
            };
            let resource = identity("resource-1", 3);
            let presented_resource = if row[4] == "true" {
                resource.clone()
            } else {
                identity("resource-1", 2)
            };
            let lease_identity = identity("lease-1", 4);
            let referenced_lease = if row[5] == "true" {
                lease_identity.clone()
            } else {
                identity("lease-1", 3)
            };
            let lease = Lease {
                identity: lease_identity,
                holder: holder.clone(),
                resources: vec![resource],
                state,
                fence_token: 9,
                expires_at_unix_ms: Some(row[7].parse().unwrap()),
            };
            let now = row[9].parse().unwrap();
            let actual = match row[1] {
                "lease" => lease.authorizes(
                    &presented_holder,
                    &presented_resource,
                    if row[6] == "true" { 9 } else { 8 },
                    now,
                ),
                "grant" => EndpointGrant {
                    identity: identity("grant-1", 1),
                    endpoint: identity("endpoint-1", 1),
                    grantee: holder,
                    lease: referenced_lease,
                    fence_token: if row[6] == "true" { 9 } else { 8 },
                    expires_at_unix_ms: row[8].parse().unwrap(),
                }
                .authorizes(
                    &if row[4] == "true" {
                        identity("endpoint-1", 1)
                    } else {
                        identity("endpoint-1", 2)
                    },
                    &presented_holder,
                    &lease,
                    now,
                ),
                kind => panic!("unknown authority kind: {kind}"),
            };
            assert_eq!(actual, row[10] == "true", "{}", row[0]);
        }

        for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/renewal.tsv")) {
            let lease = Lease {
                identity: identity("lease-1", 4),
                holder: identity("worker-holder", 2),
                resources: vec![identity("resource-1", 3)],
                state: lease_state(row[1]),
                fence_token: 9,
                expires_at_unix_ms: Some(row[3].parse().unwrap()),
            };
            let actual = lease
                .renew(
                    if row[2] == "true" { 9 } else { 8 },
                    row[4].parse().unwrap(),
                    row[5].parse().unwrap(),
                )
                .map(|_| "ACCEPT")
                .unwrap_or_else(|error| error.reason_code);
            assert_eq!(actual, row[6], "{}", row[0]);
        }

        for row in fixture_rows(include_str!("../../../tck/kernel-semantic/v1/replay.tsv")) {
            let source = identity("node-1", 7);
            let cursor = EventCursor {
                source: if row[1] == "true" {
                    source.clone()
                } else {
                    identity("node-1", 6)
                },
                sequence: row[2].parse().unwrap(),
            };
            let status = cursor.status_against(&source, row[3].parse().unwrap());
            let actual_status = match status {
                ReplayStatus::Current => "CURRENT",
                ReplayStatus::Gap => "GAP",
                ReplayStatus::SourceChanged => "SOURCE_CHANGED",
            };
            let sequences = if status == ReplayStatus::Current {
                let start = cursor
                    .sequence
                    .saturating_add(1)
                    .max(row[3].parse().unwrap());
                (start..=row[4].parse().unwrap())
                    .take(row[5].parse().unwrap())
                    .collect::<Vec<u64>>()
            } else {
                Vec::new()
            };
            let actual_sequences = if sequences.is_empty() {
                "-".to_string()
            } else {
                sequences
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            assert_eq!(
                (actual_status, actual_sequences.as_str()),
                (row[6], row[7]),
                "{}",
                row[0]
            );
        }
    }
}
