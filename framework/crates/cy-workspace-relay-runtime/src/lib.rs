//! Authenticated inbound Workspace Relay runtime for trusted Platform hosts.
//!
//! This crate owns the Tonic Relay service, connector peer validation, and
//! transport-bound certificate adapters. Workspace identity and authorization
//! facts come from `cy-workspace-control-plane`; this crate does not persist
//! Workspace or Product state.

#![forbid(unsafe_code)]

pub mod aca_forwarded_bff_workload;
pub mod aca_forwarded_certificate;
pub mod device_auth;
pub mod relay;
pub mod relay_peer_certificate_validation;

pub use aca_forwarded_bff_workload::{
    AcaForwardedBffWorkloadCertificateAdapter, BffWorkloadCertificateError,
    BffWorkloadCertificatePin, TonicBffWorkloadCertificateAdapter, VerifiedBffWorkloadIdentity,
};
pub use aca_forwarded_certificate::{
    AcaForwardedCertificateAdapter, AcaForwardedCertificateConfigError,
};
pub use cy_workspace_control_plane::*;
pub use device_auth::{
    RegistryWorkspaceDeviceVerifier, VerifiedClientCertificate, WorkspaceDeviceAuthenticationError,
};
pub use relay::WorkspaceRelay;
pub use relay_peer_certificate_validation::AuthenticatedRelayWorkspaceDevice;
pub use relay_peer_certificate_validation::{
    CurrentRelayPeerRevocationEvidence, RelayPeerCertificateConfigError, RelayPeerCertificateError,
    RelayPeerCertificateRevocationChecker, RelayPeerCertificateValidator,
    RelayPeerRevocationCheckError, RelayPeerRevocationQuery,
};
