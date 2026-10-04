//! PostgreSQL persistence adapters for the Workspace control-plane.
//!
//! Schema migration and runtime credentials are separate operations. Implementations fail closed
//! when PostgreSQL or schema validation is unavailable.

#![forbid(unsafe_code)]

pub mod authority_execution_target;
pub mod authority_web_session;
pub mod device_authorization;
pub mod device_authorization_postgres;
mod device_certificate_issuance_binding;
pub mod device_certificate_validation;
pub mod device_csr_validator;
pub mod device_enrollment_authorization_service;
pub mod device_enrollment_http;
pub mod device_enrollment_registration_postgres;
pub mod device_registry_postgres;
pub mod durable_directory;
pub mod outbox;
pub mod restricted_device_ca;
pub mod user_code_attempt_limiter_postgres;
pub mod webauthn_credential_store;
pub mod webauthn_http;
pub mod webauthn_http_binding_postgres;
pub mod webauthn_postgres_store;
pub mod webauthn_verifier;

pub use authority_execution_target::{
    AuthorityExecutionTargetBinding, AuthorityExecutionTargetError, AuthorityExecutionTargetImport,
    PostgresAuthorityExecutionTargetAdmin, PostgresAuthorityExecutionTargetStore,
};
pub use authority_web_session::{
    AuthorityWebSession, AuthorityWebSessionError, PostgresAuthorityWebSessionStore,
};
pub use cy_workspace_control_plane::{
    device_registry, directory, UserCodeKeyRing, UserCodeSecretError, VersionedUserCodeDigest,
    MAX_USER_CODE_KEY_VERSIONS,
};
pub use device_authorization::*;
pub use device_authorization_postgres::{
    DeviceAuthorizationPostgresError, PostgresDeviceAuthorizationStore,
};
pub use device_certificate_validation::*;
pub use device_csr_validator::ProductionDeviceCsrValidator;
pub use device_enrollment_authorization_service::{
    DeviceCertificatePublicMetadata, DeviceCertificatePublicMetadataPort,
    DeviceEnrollmentAuthorizationService, DeviceEnrollmentAuthorizationServiceConfig,
    DeviceEnrollmentReconciliationReport,
};
pub use device_enrollment_http::*;
pub use device_enrollment_registration_postgres::{
    DeviceEnrollmentRegistrationPostgresError, PostgresDeviceEnrollmentRegistrationTransaction,
};
pub use device_registry_postgres::{
    DeviceCertificateRegistryActivation, DeviceRegistryPostgresError,
    PostgresWorkspaceDeviceRegistry,
};
pub use durable_directory::{
    AuthenticatedWorkspaceDevice, DeviceRegistrationBinding, DeviceRegistrationError,
    DirectoryMutation, DirectoryOperatorProvisioner, DurableDirectoryError,
    PostgresDeviceRegistrationAuthority, PostgresWorkspaceDirectory,
    WorkspaceDeviceRegistrationAuthority, SUPPORTED_OPERATOR_ROLES,
    WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE,
};
pub use outbox::*;
pub use restricted_device_ca::{
    PostgresRelayPeerSignedCrlChecker, PostgresRestrictedDeviceCa, PostgresSignedCrlChecker,
    RestrictedDeviceCaError,
};
pub use user_code_attempt_limiter_postgres::PostgresUserCodeAttemptReservation;
pub use webauthn_credential_store::*;
pub use webauthn_http::*;
pub use webauthn_http_binding_postgres::{
    PostgresWebAuthnHttpSessionBindingStore, WebAuthnHttpSessionBindingPostgresError,
};
pub use webauthn_postgres_store::{PostgresWebAuthnCredentialStore, WebAuthnPostgresStoreError};
pub use webauthn_verifier::*;
