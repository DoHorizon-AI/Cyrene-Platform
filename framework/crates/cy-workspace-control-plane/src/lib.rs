//! Trusted Workspace identity, authorization, and control-plane implementation.
//!
//! Host code supplies configured persistence and transport adapters. This crate does not create
//! trusted caller identities from untrusted request fields.

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod authority_service;
pub mod caller;
pub mod connector_session;
pub mod control_plane;
pub mod device_registry;
pub mod direct_server;
pub mod directory;
pub mod persistent_directory;
pub mod product_authorization;
pub mod product_projection;
pub mod user_code_secret;
pub mod web_identity;
pub mod web_relay_session;

pub use api::{
    bounded_workspace_direct_server, bounded_workspace_relay_server,
    dispatch_authenticated_workspace_request, dispatch_workspace_request,
    unauthenticated_workspace_response, LocalWorkspaceClient, WorkspaceApi,
};
pub use auth::{
    DevelopmentSessionVerifier, RelayAuthenticationError, RelayAuthenticator, RelaySessionClaims,
    SessionPrincipal,
};
pub use authority_service::*;
pub use caller::{
    WorkspaceAuthorizationError, WorkspaceCallerContext, WorkspaceCallerContextError,
    WorkspaceCallerPrincipal, WorkspaceDeviceIdentity, NAVIGATOR_SERVICE_WRITER_ROLE,
    WORKSPACE_MEMBER_ROLE,
};
pub use connector_session::{
    connect_discovered_workspace, connect_relay_serving_session, run_workspace_connector_session,
    RelayClientConfig, RelayServingSession, RelayTransportError, WorkspaceConnection,
};
/// Compatibility name for the authenticated Connector-serving session.
pub type RelaySession = RelayServingSession;
/// Compatibility entry point for existing Connector host compositions.
pub use connector_session::connect_relay_serving_session as connect_relay_session;
pub use control_plane::{
    WorkspaceControlPlane, WorkspaceControlPlaneConfigError, WorkspaceDispatchError,
    WorkspaceOperationProjection, WorkspaceProductRequest, WorkspaceRequestDispatcher,
};
pub use cy_proto::workspace_v1;
pub use cy_workspace_client_sdk::WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES;
pub use device_registry::{
    ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus,
    WorkspaceDeviceCertificateIdentity, WorkspaceDeviceDispatchFence, WorkspaceDeviceKey,
    WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
};
pub use direct_server::DirectWorkspaceServer;
pub use directory::{
    validate_descriptor, InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError,
    WorkspaceMembership,
};
pub use persistent_directory::FileWorkspaceDirectory;
pub use product_projection::PRODUCT_JSON_BODY_MAX_BYTES;
pub use user_code_secret::{
    UserCodeKeyRing, UserCodeSecretError, VersionedUserCodeDigest, MAX_USER_CODE_KEY_VERSIONS,
};
#[cfg(feature = "test-support")]
pub use web_identity::test_principal;
pub use web_identity::{
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, VerifiedWebPrincipal,
    WebIdentityDirectory, WebIdentityDirectoryError, WebIdentityError, WebPrincipalVerifier,
};
pub use web_relay_session::{
    WebRelaySessionCredentialIssuer, WebRelaySessionError, WebRelaySessionIssuer,
    WebRelaySessionVerifier,
};
