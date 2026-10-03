//! Legacy Workspace Fabric V1 compatibility facade.
//!
//! Client-side callers should depend on `cy-workspace-client-sdk`. The former Platform
//! Connector and Relay processes have been removed; this facade remains for the V1 fixture and
//! protocol checks. Platform Authority services depend on their control-plane and storage crates
//! directly.

#![forbid(unsafe_code)]

mod frontend_relay_client;

pub use cy_workspace_control_plane::*;
pub use cy_workspace_relay_runtime::*;
pub use frontend_relay_client::{
    FrontendRelayClient, FrontendRelayClientConfig, FrontendRelayClientError,
    FrontendWorkspaceTransport,
};
