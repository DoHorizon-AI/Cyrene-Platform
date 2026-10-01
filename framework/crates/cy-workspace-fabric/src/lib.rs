//! Workspace Fabric compatibility facade.
//!
//! Client-side callers should depend on `cy-workspace-client-sdk`. Platform hosts use the
//! control-plane and authenticated Relay runtime crates directly; this facade remains for the
//! connector's server-serving transport and existing composition entry points.

#![forbid(unsafe_code)]

mod frontend_relay_client;

pub use cy_workspace_control_plane::*;
pub use cy_workspace_relay_runtime::*;
pub use frontend_relay_client::{
    FrontendRelayClient, FrontendRelayClientConfig, FrontendRelayClientError,
    FrontendWorkspaceTransport,
};
