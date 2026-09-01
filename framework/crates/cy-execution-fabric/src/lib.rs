//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_execution_fabric                                        │
//! │  Role: Product-neutral attachment admission and reconciliation.     │
//! │                                                                     │
//! │  模块职责：执行 Attachment 的准入、generation fencing 与证据协调。      │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod admission;
mod connectivity;
mod enrollment;
mod reconcile;

pub use admission::{
    validate_assignment, validate_hello, validate_renewal, AdmissionDisposition,
    FabricContractError, ObservationCursor,
};
pub use connectivity::{ConnectivityProvider, ConnectivityRoute, DirectConnectivityProvider};
pub use enrollment::{
    DevelopmentEnrollmentProvider, EnrollmentGrant, EnrollmentProvider, RuntimeScope,
};
pub use reconcile::{
    reconcile_runtime, DesiredRuntime, LeaseObservation, ProviderObservation, ReconcileEvidence,
    RuntimeDisposition,
};
