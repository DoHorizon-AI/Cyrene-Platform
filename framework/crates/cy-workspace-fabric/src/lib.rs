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
mod caller;
mod control_plane;
mod device_registry;
mod direct;
mod directory;
mod persistent_directory;
// The policy slice lands before its separately owned caller/runtime call site.
#[allow(dead_code)]
pub(crate) mod product_authorization;
pub mod device_authorization;
mod product_projection;
mod relay;
mod transport;

pub use api::{
    bounded_workspace_direct_server, bounded_workspace_relay_server, LocalWorkspaceClient,
    WorkspaceApi,
};
pub use auth::{
    DevelopmentSessionVerifier, RelayAuthenticator, RelaySessionClaims, SessionPrincipal,
};
pub use caller::{
    WorkspaceAuthorizationError, WorkspaceCallerContext, WorkspaceCallerPrincipal,
    WorkspaceDeviceIdentity, NAVIGATOR_SERVICE_WRITER_ROLE, WORKSPACE_MEMBER_ROLE,
};
pub use control_plane::{
    WorkspaceControlPlane, WorkspaceControlPlaneConfigError, WorkspaceDispatchError,
    WorkspaceOperationProjection, WorkspaceProductRequest, WorkspaceRequestDispatcher,
};
pub use device_registry::{
    ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus, WorkspaceDeviceKey,
    WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
};
pub use direct::DirectWorkspaceServer;
pub use directory::{
    validate_descriptor, InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError,
    WorkspaceMembership,
};
pub use persistent_directory::FileWorkspaceDirectory;
pub use product_projection::{
    ProductInvocationError, ProductInvocationPort, ProductInvocationRequest,
    ProductInvocationResponse, PRODUCT_JSON_BODY_MAX_BYTES, WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES,
};
pub use relay::WorkspaceRelay;
pub use transport::{
    connect_discovered_workspace, connect_relay_session, run_workspace_connector_session,
    RelayClientConfig, RelaySession, WorkspaceConnection,
};

pub use cy_proto::workspace_v1;
