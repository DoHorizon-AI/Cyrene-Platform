//! Loopback Workspace bridge implementation and local bearer authentication.
//!
//! The process-facing API lives here so the Sidecar binary depends on the client SDK rather
//! than Platform control-plane, database, or WebAuthn implementation crates.

#![forbid(unsafe_code)]

mod sidecar;

pub use sidecar::{LocalBearerInterceptor, SidecarConfigurationError, WorkspaceSidecar};
