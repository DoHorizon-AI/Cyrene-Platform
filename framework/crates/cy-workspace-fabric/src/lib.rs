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
mod direct;
mod directory;
mod persistent_directory;
mod relay;
mod transport;

pub use api::{LocalWorkspaceClient, WorkspaceApi};
pub use auth::{
    DevelopmentSessionVerifier, RelayAuthenticator, RelaySessionClaims, SessionPrincipal,
};
pub use direct::DirectWorkspaceServer;
pub use directory::{
    validate_descriptor, InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError,
    WorkspaceMembership,
};
pub use persistent_directory::FileWorkspaceDirectory;
pub use relay::WorkspaceRelay;
pub use transport::{
    connect_discovered_workspace, connect_relay_session, run_workspace_connector_session,
    RelayClientConfig, RelaySession, WorkspaceConnection,
};

pub use cy_proto::workspace_v1;
