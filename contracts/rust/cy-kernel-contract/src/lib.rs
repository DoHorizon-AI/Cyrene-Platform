//! Pure, transport-independent projection of CYRENE Kernel Semantic Contract v1.
//!
//! This crate contains no I/O, operating-system handles, serialization or
//! implementation-specific runtime types. Transport and language bindings map
//! to these semantics rather than making their own semantics.

#![forbid(unsafe_code)]

pub(crate) mod endpoint;
pub(crate) mod event;
pub(crate) mod identity;
pub(crate) mod lease;
pub(crate) mod operation;
pub(crate) mod resource;
pub(crate) mod revision;
pub(crate) mod snapshot;
pub(crate) mod validation;
pub(crate) mod worker;

#[cfg(test)]
mod tests;

pub use endpoint::{Endpoint, EndpointGrant};
pub use event::{Event, EventCursor, EventPage, ReplayStatus};
pub use identity::{Identity, Principal};
pub use lease::{Lease, LeaseState};
pub use operation::{Operation, OperationState};
pub use resource::{
    Capability, CapabilityRequirement, Provider, ProviderState, Quantity, Resource, ResourceQuery,
    ResourceState, TopologyLink,
};
pub use revision::{ContractRevision, Rejection};
pub use snapshot::{ProviderSnapshot, SemanticAction, V1_ACTIONS};
pub use validation::{
    ContractError, CONTRACT_ID, CONTRACT_MAJOR, CONTRACT_MINOR, CONTRACT_VERSION, MAX_CAPABILITIES,
    MAX_ENDPOINTS_PER_SNAPSHOT, MAX_ERROR_MESSAGE_BYTES, MAX_EVENTS_PER_PAGE, MAX_EVENT_BODY_BYTES,
    MAX_EXECUTION_REF_BYTES, MAX_ID_BYTES, MAX_NAMESPACED_ID_BYTES, MAX_PROPERTIES,
    MAX_RESOURCES_PER_LEASE, MAX_RESOURCES_PER_SNAPSHOT, MAX_TIMESTAMP_UNIX_MS,
    MAX_WORKERS_PER_SNAPSHOT,
};
pub use worker::{Worker, WorkerState};
