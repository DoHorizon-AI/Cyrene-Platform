//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_execution_fabric                                        │
//! │  Role: Product-neutral attachment admission and reconciliation.     │
//! │                                                                     │
//! │  模块职责：执行 Attachment 的准入、generation fencing 与证据协调。      │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod admission;
mod assignment;
mod capability;
mod connectivity;
mod enrollment;
mod node;
mod placement;
mod provider;
mod reconcile;

pub use admission::{
    semantic_lease_from_proto, validate_assignment, validate_hello, validate_renewal,
    AdmissionDisposition, FabricContractError, ObservationCursor,
};
pub use assignment::RuntimeAssignmentBuilder;
pub use capability::{
    artifact_transfer_capability, execution_capability, validate_execution_capability,
    ExecutionCapabilityEnvelope, ARTIFACT_TRANSFER_CAPABILITY_ID, EXECUTION_CAPABILITY_ID,
};
pub use connectivity::{
    ConnectivityProvider, ConnectivityRoute, LocalConnectivityProvider, RelayConnectivityProvider,
};
pub use enrollment::{
    DevelopmentEnrollmentProvider, EnrollmentGrant, EnrollmentProvider, RuntimeScope,
};
pub use node::NodeLifecycleProjection;
pub use placement::{
    place_execution_target, plan_execution_placement, ArtifactAvailability, ArtifactPlacementQuote,
    ArtifactTransferQuote, CandidateEvaluation, ExecutionPlacementRequest,
    ExecutionTargetCandidate, NetworkRequirements, PlacementDecision, PlacementPolicy,
    PlacementReason, PlacementScore, ResourceMatchEvidence,
};
pub use provider::{FakeProvider, ProviderObservationSource};
pub use reconcile::{
    reconcile_runtime, DesiredRuntime, LeaseObservation, ProviderObservation, ReconcileEvidence,
    RuntimeDisposition,
};
