//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_workspace_fabric                                        │
//! │  Role: Workspace discovery and outbound relay connectivity.         │
//! │                                                                     │
//! │  模块职责：Workspace 发现、连接描述符与出站 Relay 连接。                 │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod aca_forwarded_bff_workload;
mod aca_forwarded_certificate;
mod api;
mod auth;
mod caller;
mod control_plane;
mod device_auth;
mod device_csr_validator;
pub mod device_enrollment_http;
mod device_registry;
mod device_registry_postgres;
mod direct;
mod directory;
mod durable_directory;
mod frontend_relay_client;
mod persistent_directory;
// The policy slice lands before its separately owned caller/runtime call site.
pub mod device_authorization;
mod device_authorization_postgres;
pub mod device_enrollment_authorization_service;
mod device_enrollment_registration_postgres;
pub mod user_code_attempt_limiter_postgres;
// The Directory-bound consumer lands separately; keep this staged port
// available without wiring a production signer that does not exist yet.
#[allow(dead_code)]
pub(crate) mod device_certificate_authority;
// This verifier also remains disconnected until a CA, trust roots, and a
// current revocation-status provider are configured.
#[allow(dead_code)]
pub(crate) mod device_certificate_validation;
// This inbound peer validator remains disconnected until Relay supplies a
// trusted transport identity source and a current revocation adapter.
mod product_adapters;
#[allow(dead_code)]
pub(crate) mod product_authorization;
mod product_projection;
mod relay;
#[allow(dead_code)]
pub(crate) mod relay_peer_certificate_validation;
mod sidecar;
mod transport;
mod user_code_secret;
mod web_identity;
mod web_relay_session;
// The owner adapter and Relay host callsite are integrated separately; until
// then this reusable sweep service is deliberately not started by any host.
#[allow(dead_code)]
pub(crate) mod device_authorization_sweeper;
pub mod webauthn_credential_store;
pub mod webauthn_http;
mod webauthn_http_binding_postgres;
mod webauthn_postgres_store;
pub mod webauthn_verifier;

pub use aca_forwarded_bff_workload::{
    AcaForwardedBffWorkloadCertificateAdapter, BffWorkloadCertificateError,
    BffWorkloadCertificatePin, VerifiedBffWorkloadIdentity,
};
pub use aca_forwarded_certificate::{
    AcaForwardedCertificateAdapter, AcaForwardedCertificateConfigError,
};
pub use api::{
    bounded_workspace_direct_server, bounded_workspace_relay_server, LocalWorkspaceClient,
    WorkspaceApi,
};
pub use auth::{
    DevelopmentSessionVerifier, RelayAuthenticationError, RelayAuthenticator, RelaySessionClaims,
    SessionPrincipal,
};
pub use caller::{
    WorkspaceAuthorizationError, WorkspaceCallerContext, WorkspaceCallerPrincipal,
    WorkspaceDeviceIdentity, NAVIGATOR_SERVICE_WRITER_ROLE, WORKSPACE_MEMBER_ROLE,
};
pub use control_plane::{
    WorkspaceControlPlane, WorkspaceControlPlaneConfigError, WorkspaceDispatchError,
    WorkspaceOperationProjection, WorkspaceProductRequest, WorkspaceRequestDispatcher,
};
pub use device_auth::{
    RegistryWorkspaceDeviceVerifier, VerifiedClientCertificate, WorkspaceDeviceAuthenticationError,
};
pub use device_authorization_postgres::{
    DeviceAuthorizationPostgresError, PostgresDeviceAuthorizationStore,
};
pub use device_csr_validator::ProductionDeviceCsrValidator;
pub use device_enrollment_authorization_service::{
    DeviceCertificatePublicMetadata, DeviceCertificatePublicMetadataPort,
    DeviceEnrollmentAuthorizationService, DeviceEnrollmentAuthorizationServiceConfig,
};
pub use device_enrollment_http::{
    device_enrollment_v1_router, DeviceEnrollmentAuthorizationPort,
    DeviceEnrollmentHttpDependencies, DeviceEnrollmentHttpError,
    DeviceEnrollmentRegistrationTransactionPort, SecretBytes, TrustedEnrollmentAbuseKey,
    TrustedInteractiveUserSession,
};
pub use device_enrollment_registration_postgres::{
    DeviceEnrollmentRegistrationPostgresError, PostgresDeviceEnrollmentRegistrationTransaction,
};
pub use device_registry::{
    ApprovedWorkspaceDeviceCertificate, DeviceAuthorizationStatus,
    WorkspaceDeviceCertificateIdentity, WorkspaceDeviceDispatchFence, WorkspaceDeviceKey,
    WorkspaceDeviceRecord, WorkspaceDeviceRegistry,
};
pub use device_registry_postgres::{
    DeviceCertificateRegistryActivation, DeviceRegistryPostgresError,
    PostgresWorkspaceDeviceRegistry,
};
pub use direct::DirectWorkspaceServer;
pub use directory::{
    validate_descriptor, InMemoryWorkspaceDirectory, WorkspaceDirectory, WorkspaceDirectoryError,
    WorkspaceMembership,
};
pub use durable_directory::{
    AuthenticatedWorkspaceDevice, DeviceRegistrationBinding, DeviceRegistrationError,
    DirectoryMutation, DirectoryOperatorProvisioner, DurableDirectoryError,
    PostgresDeviceRegistrationAuthority, PostgresWorkspaceDirectory,
    WorkspaceDeviceRegistrationAuthority, SUPPORTED_OPERATOR_ROLES,
    WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE,
};
pub use frontend_relay_client::{
    FrontendRelayClient, FrontendRelayClientConfig, FrontendRelayClientError,
    FrontendWorkspaceTransport,
};
pub use persistent_directory::FileWorkspaceDirectory;
pub use product_adapters::{
    load_product_endpoint_configs, load_product_endpoint_configs_for_workspace,
    CatalystEchoProductApiAdapter, ProductEndpointConfig, ProductEndpointManifestError,
    ProductHttpApiAdapter, ProductHttpClient,
};
pub use product_projection::{
    ProductInvocationError, ProductInvocationPort, ProductInvocationRequest,
    ProductInvocationResponse, PRODUCT_JSON_BODY_MAX_BYTES, WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES,
};
pub use relay::WorkspaceRelay;
pub use sidecar::{LocalBearerInterceptor, SidecarConfigurationError, WorkspaceSidecar};
pub use transport::{
    connect_discovered_workspace, connect_relay_session, run_workspace_connector_session,
    RelayClientConfig, RelaySession, WorkspaceConnection,
};
pub use user_code_secret::{
    UserCodeKeyRing, UserCodeSecretError, VersionedUserCodeDigest, MAX_USER_CODE_KEY_VERSIONS,
};
pub use web_identity::{
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, VerifiedWebPrincipal,
    WebIdentityDirectory, WebIdentityDirectoryError, WebIdentityError, WebPrincipalVerifier,
};
pub use web_relay_session::{
    WebRelaySessionCredentialIssuer, WebRelaySessionError, WebRelaySessionIssuer,
    WebRelaySessionVerifier,
};
pub use webauthn_credential_store::{
    WebAuthnAuthenticationCommit, WebAuthnCredentialStore, WebAuthnCredentialStoreError,
};
pub use webauthn_http::{
    authorize_verified_web_session_role, webauthn_http_router, VerifiedWebSessionContext,
    WebAuthnHttpAuthorizationError, WebAuthnHttpCeremonyPurpose, WebAuthnHttpConfigurationError,
    WebAuthnHttpSessionBindingError, WebAuthnHttpSessionBindingStore, WebAuthnHttpState,
    WebAuthnSessionBindingDigest, WebAuthnSessionCeremonyBinding, WebAuthnSessionContextError,
    WebAuthnSessionFinishReservation,
};
pub use webauthn_http_binding_postgres::{
    PostgresWebAuthnHttpSessionBindingStore, WebAuthnHttpSessionBindingPostgresError,
};
pub use webauthn_postgres_store::{PostgresWebAuthnCredentialStore, WebAuthnPostgresStoreError};
pub use webauthn_verifier::{
    SqliteWebAuthnCredentialStore, VerifiedWebAuthnPasskey, WebAuthnAuditAction,
    WebAuthnAuditEvent, WebAuthnAuditFailure, WebAuthnAuthenticationVerifier,
    WebAuthnBackupCredentialPolicy, WebAuthnCredentialEnrollmentAuthorizer,
    WebAuthnCredentialEnrollmentService, WebAuthnCredentialManagementAction,
    WebAuthnCredentialRegistrationChallenge, WebAuthnCredentialRevocationReason,
    WebAuthnVerifierConfig, WebAuthnVerifierConfigError, WebAuthnVerifierSystemClock,
};

pub use cy_proto::workspace_v1;
