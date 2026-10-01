//! Lightweight Workspace client API for discovery, mTLS connections, and Relay calls.
//!
//! This crate contains client transport code only. Workspace server handlers, identity
//! verification, authorization policy, and persistence stay in Platform host crates.

#![forbid(unsafe_code)]

mod descriptor;
mod transport;

/// Maximum encoded Workspace gRPC request or response size accepted by clients.
pub const WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

pub use descriptor::{validate_workspace_descriptor, WorkspaceDescriptorValidationError};
pub use transport::{
    connect_discovered_workspace, connect_relay_session, RelayClientConfig, RelaySession,
    RelayTransportError, WorkspaceConnection,
};
