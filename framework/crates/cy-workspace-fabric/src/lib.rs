//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_workspace_fabric                                        │
//! │  Role: Workspace discovery and outbound relay connectivity.         │
//! │                                                                     │
//! │  模块职责：Workspace 发现、连接描述符与出站 Relay 连接。                 │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod api;
mod auth;
mod directory;
mod relay;
mod transport;

pub use api::{LocalWorkspaceClient, WorkspaceApi};
pub use auth::{
    DevelopmentSessionVerifier, RelayAuthenticator, RelaySessionClaims, SessionPrincipal,
};
pub use directory::{
    validate_descriptor, InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceMembership,
};
pub use relay::WorkspaceRelay;
pub use transport::{
    connect_relay_session, run_workspace_connector_session, RelayClientConfig, RelaySession,
};

pub use cy_proto::workspace_v1;
