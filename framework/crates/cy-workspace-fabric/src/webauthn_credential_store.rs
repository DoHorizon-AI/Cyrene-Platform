//! Storage contract shared by the WebAuthn assertion verifier and credential registrar.
//!
//! Implementations must persist credential ownership, ceremony state, replay
//! records and audit events. Successful assertion consumption and signature
//! counter advancement must be atomic. This crate currently provides only the
//! SQLite adapter, which is limited to local and single-instance deployments.
//! Production multi-replica deployments need a shared transactional adapter.

use cy_proto::workspace_v1::UserIdentityRef;
use webauthn_rs::prelude::Passkey;

use crate::device_authorization::{DeviceAuthorizationId, WebAuthnAuthenticationContext};
pub use crate::webauthn_verifier::{
    VerifiedWebAuthnPasskey, WebAuthnAuditEvent, WebAuthnAuditFailure,
    WebAuthnCredentialRevocationReason,
};

/// Errors from the durable WebAuthn credential and ceremony repository.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnCredentialStoreError {
    #[error("WebAuthn credential store is unavailable or corrupt")]
    Unavailable,
    #[error("WebAuthn credential record is invalid")]
    InvalidRecord,
    #[error("WebAuthn credential is already registered")]
    CredentialAlreadyRegistered,
    #[error("WebAuthn user handle conflicts with another owner")]
    UserHandleConflict,
    #[error("WebAuthn ceremony conflicts with an existing attempt")]
    CeremonyConflict,
    #[error("WebAuthn credential was not found")]
    CredentialNotFound,
    #[error("WebAuthn store path must be a private regular file in an existing directory")]
    InvalidPath,
    #[error("WebAuthn store file or directory permissions are not private")]
    InsecurePermissions,
}

/// A passkey snapshot tied to one exact owner and credential ID.
#[derive(Clone)]
pub struct StoredWebAuthnCredential {
    pub credential_id: Vec<u8>,
    pub passkey: Passkey,
    pub signature_counter: u32,
    pub serialized_sha256: [u8; 32],
}

/// All active passkeys for one trusted external identity.
#[derive(Clone)]
pub struct StoredWebAuthnCredentialSet {
    pub user_handle: Vec<u8>,
    pub credentials: Vec<StoredWebAuthnCredential>,
}

/// Durable state for one in-progress authentication ceremony.
pub struct PersistedWebAuthnCeremony {
    pub owner: UserIdentityRef,
    pub authorization_id: DeviceAuthorizationId,
    pub user_handle: Vec<u8>,
    pub context_sha256: [u8; 32],
    pub opaque_state: Vec<u8>,
    pub state_sha256: [u8; 32],
    pub credential_set_sha256: [u8; 32],
    pub expires_at_unix_ms: u64,
    pub consumed_assertion_sha256: Option<[u8; 32]>,
    pub consumed_credential_id: Option<Vec<u8>>,
}

/// Durable state for one in-progress passkey registration ceremony.
pub struct PersistedWebAuthnRegistration {
    pub owner: UserIdentityRef,
    pub user_handle: Vec<u8>,
    pub opaque_state: Vec<u8>,
    pub state_sha256: [u8; 32],
    pub expires_at_unix_ms: u64,
    pub consumed_response_sha256: Option<[u8; 32]>,
    pub consumed_credential_id: Option<Vec<u8>>,
}

/// Storage boundary for the verifier and protected registrar.
pub trait WebAuthnCredentialStore: Send + Sync {
    fn ensure_user_handle(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<Vec<u8>, WebAuthnCredentialStoreError>;

    fn credentials_for_owner(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<Option<StoredWebAuthnCredentialSet>, WebAuthnCredentialStoreError>;

    fn begin_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        user_handle: &[u8],
        opaque_state: &[u8],
        credential_set_sha256: [u8; 32],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError>;

    fn authentication_ceremony(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<PersistedWebAuthnCeremony>, WebAuthnCredentialStoreError>;

    fn commit_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        opaque_state: &[u8],
        assertion_sha256: [u8; 32],
        credential_id: &[u8],
        expected_counter: u32,
        expected_passkey_sha256: [u8; 32],
        updated_passkey: &Passkey,
        now_unix_ms: u64,
        user_verified: bool,
        backup_eligible: bool,
        backup_state: bool,
    ) -> Result<(), WebAuthnCredentialStoreError>;

    fn begin_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        user_handle: &[u8],
        opaque_state: &[u8],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError>;

    fn registration_ceremony(
        &self,
        registration_id: &DeviceAuthorizationId,
    ) -> Result<Option<PersistedWebAuthnRegistration>, WebAuthnCredentialStoreError>;

    fn commit_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        opaque_state: &[u8],
        response_sha256: [u8; 32],
        credential: &VerifiedWebAuthnPasskey,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError>;

    fn revoke_credential(
        &self,
        owner: &UserIdentityRef,
        credential_id: &[u8],
        reason: WebAuthnCredentialRevocationReason,
        now_unix_ms: u64,
    ) -> Result<bool, WebAuthnCredentialStoreError>;

    fn record_rejection(
        &self,
        owner: &UserIdentityRef,
        correlation_id: &DeviceAuthorizationId,
        credential_id_sha256: Option<[u8; 32]>,
        reason: WebAuthnAuditFailure,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError>;

    fn audit_events(
        &self,
        owner: &UserIdentityRef,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<WebAuthnAuditEvent>, WebAuthnCredentialStoreError>;
}
