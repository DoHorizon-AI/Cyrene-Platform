//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 webauthn_verifier.rs                                            │
//! │  Module: cy_workspace_fabric::webauthn_verifier                     │
//! │  Role: Production WebAuthn assertion verification and SQLite state. │
//! │                                                                     │
//! │  模块职责：固定 RP 配置的 WebAuthn assertion verifier 与持久存储。     │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! The verifier binds each ceremony to its server-side context and registered
//! credential owner. The SQLite adapter atomically consumes successful
//! ceremonies with the corresponding credential counter update. Opaque
//! `PasskeyAuthentication` state stays server-side and is never logged or
//! returned to a browser.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_v1::UserIdentityRef;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    Credential, Passkey, PasskeyAuthentication, PublicKeyCredential, RegisterPublicKeyCredential,
    Webauthn, WebauthnBuilder,
};

pub use crate::webauthn_credential_store::{
    PersistedWebAuthnCeremony, PersistedWebAuthnRegistration, StoredWebAuthnCredential,
    StoredWebAuthnCredentialSet, WebAuthnAuthenticationCommit, WebAuthnCredentialStore,
    WebAuthnCredentialStoreError,
};

#[cfg(test)]
use crate::device_authorization::DeviceAuthorizationDeviceKey;
use crate::device_authorization::{
    DeviceAuthorizationClockPort, DeviceAuthorizationId, DeviceAuthorizationPortError,
    WebAuthnAuthenticationContext, WebAuthnAuthenticationPort, WebAuthnAuthenticationStart,
};

const DEFAULT_CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CEREMONY_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_ASSERTION_BYTES: usize = 16 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024;
const MIN_USER_HANDLE_BYTES: usize = 16;
const MAX_USER_HANDLE_BYTES: usize = 64;
const MAX_IDENTITY_FIELD_BYTES: usize = 4096;
const AUDIT_PAGE_LIMIT: usize = 1000;

/// Rejection reason persisted to the credential audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnAuditFailure {
    StateOrOwnerMismatch,
    CeremonyExpired,
    InvalidAssertion,
    UserHandleMismatch,
    UserVerificationMissing,
    BackupCredentialDisallowed,
    SignatureCounterReplay,
    CredentialChangedOrRevoked,
    InvalidRegistration,
}

impl WebAuthnAuditFailure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::StateOrOwnerMismatch => "STATE_OR_OWNER_MISMATCH",
            Self::CeremonyExpired => "CEREMONY_EXPIRED",
            Self::InvalidAssertion => "INVALID_ASSERTION",
            Self::UserHandleMismatch => "USER_HANDLE_MISMATCH",
            Self::UserVerificationMissing => "USER_VERIFICATION_MISSING",
            Self::BackupCredentialDisallowed => "BACKUP_CREDENTIAL_DISALLOWED",
            Self::SignatureCounterReplay => "SIGNATURE_COUNTER_REPLAY",
            Self::CredentialChangedOrRevoked => "CREDENTIAL_CHANGED_OR_REVOKED",
            Self::InvalidRegistration => "INVALID_REGISTRATION",
        }
    }
}

/// Lifecycle and authentication events retained by the credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnAuditAction {
    AuthenticationChallengeIssued,
    CredentialRegistrationStarted,
    CredentialRegistered,
    AssertionAccepted,
    SecurityRejected,
    CredentialRevoked,
}

impl WebAuthnAuditAction {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::AuthenticationChallengeIssued => "AUTHENTICATION_CHALLENGE_ISSUED",
            Self::CredentialRegistrationStarted => "CREDENTIAL_REGISTRATION_STARTED",
            Self::CredentialRegistered => "CREDENTIAL_REGISTERED",
            Self::AssertionAccepted => "ASSERTION_ACCEPTED",
            Self::SecurityRejected => "SECURITY_REJECTED",
            Self::CredentialRevoked => "CREDENTIAL_REVOKED",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "AUTHENTICATION_CHALLENGE_ISSUED" => Some(Self::AuthenticationChallengeIssued),
            "CREDENTIAL_REGISTRATION_STARTED" => Some(Self::CredentialRegistrationStarted),
            "CREDENTIAL_REGISTERED" => Some(Self::CredentialRegistered),
            "ASSERTION_ACCEPTED" => Some(Self::AssertionAccepted),
            "SECURITY_REJECTED" => Some(Self::SecurityRejected),
            "CREDENTIAL_REVOKED" => Some(Self::CredentialRevoked),
            _ => None,
        }
    }
}

/// Closed set of administrative reasons for removing an enrolled passkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnCredentialRevocationReason {
    UserRequest,
    SuspectedCloning,
    AccountRecovery,
    PolicyChange,
}

impl WebAuthnCredentialRevocationReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UserRequest => "USER_REQUEST",
            Self::SuspectedCloning => "SUSPECTED_CLONING",
            Self::AccountRecovery => "ACCOUNT_RECOVERY",
            Self::PolicyChange => "POLICY_CHANGE",
        }
    }
}

/// Backup-eligible credentials can sync to another device, changing the clone
/// and counter risk model. The default rejects them; allowing them is explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnBackupCredentialPolicy {
    RejectBackupEligible,
    AllowBackupEligible,
}

/// Fixed relying-party configuration for both registration and authentication.
#[derive(Clone)]
pub struct WebAuthnVerifierConfig {
    rp_id: String,
    rp_origin: Url,
    ceremony_ttl: Duration,
    backup_policy: WebAuthnBackupCredentialPolicy,
}

impl WebAuthnVerifierConfig {
    /// Create a verifier configuration bound to one HTTPS origin and RP ID.
    pub fn new(
        rp_id: impl Into<String>,
        rp_origin: Url,
    ) -> Result<Self, WebAuthnVerifierConfigError> {
        let config = Self {
            rp_id: rp_id.into(),
            rp_origin,
            ceremony_ttl: DEFAULT_CEREMONY_TTL,
            backup_policy: WebAuthnBackupCredentialPolicy::RejectBackupEligible,
        };
        config.validate()?;
        Ok(config)
    }

    /// Set one server-side expiry for browser ceremonies.
    pub fn with_ceremony_ttl(
        mut self,
        ceremony_ttl: Duration,
    ) -> Result<Self, WebAuthnVerifierConfigError> {
        if ceremony_ttl.is_zero() || ceremony_ttl > MAX_CEREMONY_TTL {
            return Err(WebAuthnVerifierConfigError::InvalidCeremonyTtl);
        }
        self.ceremony_ttl = ceremony_ttl;
        Ok(self)
    }

    /// Explicitly permit synced or backup-eligible passkeys.
    pub fn with_backup_eligible_credentials_allowed(mut self) -> Self {
        self.backup_policy = WebAuthnBackupCredentialPolicy::AllowBackupEligible;
        self
    }

    /// Return the immutable RP ID selected at service construction.
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    /// Return the one accepted browser origin selected at service construction.
    pub fn rp_origin(&self) -> &Url {
        &self.rp_origin
    }

    fn validate(&self) -> Result<(), WebAuthnVerifierConfigError> {
        if self.rp_id.trim().is_empty()
            || self.rp_id.trim() != self.rp_id
            || self.rp_id.len() > 253
            || self.rp_origin.scheme() != "https"
            || self.rp_origin.host_str().is_none()
            || self.rp_origin.username() != ""
            || self.rp_origin.password().is_some()
            || self.rp_origin.path() != "/"
            || self.rp_origin.query().is_some()
            || self.rp_origin.fragment().is_some()
            || self.ceremony_ttl.is_zero()
            || self.ceremony_ttl > MAX_CEREMONY_TTL
        {
            return Err(WebAuthnVerifierConfigError::InvalidRelyingParty);
        }
        self.build_webauthn().map(|_| ())
    }

    fn build_webauthn(&self) -> Result<Webauthn, WebAuthnVerifierConfigError> {
        WebauthnBuilder::new(&self.rp_id, &self.rp_origin)
            .map_err(|_| WebAuthnVerifierConfigError::InvalidRelyingParty)?
            .rp_name("Cyrene Workspace")
            .timeout(self.ceremony_ttl)
            .allow_subdomains(false)
            .allow_any_port(false)
            .build()
            .map_err(|_| WebAuthnVerifierConfigError::InvalidRelyingParty)
    }
}

/// Invalid or unsafe fixed WebAuthn service configuration.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnVerifierConfigError {
    #[error("WebAuthn RP ID and origin must be a fixed HTTPS origin")]
    InvalidRelyingParty,
    #[error("WebAuthn ceremony TTL must be between zero and ten minutes")]
    InvalidCeremonyTtl,
}

/// A WebAuthn passkey produced only after the fixed-RP registration check.
/// Fields are private so repositories cannot be used to import unchecked data.
pub struct VerifiedWebAuthnPasskey {
    owner: UserIdentityRef,
    user_handle: Vec<u8>,
    passkey: Passkey,
    signature_counter: u32,
    backup_eligible: bool,
    backup_state: bool,
}

impl VerifiedWebAuthnPasskey {
    /// Exact external identity bound by the authenticated registration flow.
    pub fn owner(&self) -> &UserIdentityRef {
        &self.owner
    }

    /// Passkey registration result verified against the configured RP.
    pub fn passkey(&self) -> &Passkey {
        &self.passkey
    }

    /// Stable opaque WebAuthn user handle allocated for this owner.
    pub fn user_handle(&self) -> &[u8] {
        &self.user_handle
    }

    /// Initial authenticator signature counter recorded at enrollment.
    pub fn signature_counter(&self) -> u32 {
        self.signature_counter
    }

    /// Whether the authenticator marked this credential as backup eligible.
    pub fn backup_eligible(&self) -> bool {
        self.backup_eligible
    }

    /// Whether this credential was backed up at registration time.
    pub fn backup_state(&self) -> bool {
        self.backup_state
    }
}

/// Append-only audit entry for credential lifecycle and assertion decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnAuditEvent {
    pub sequence: u64,
    pub occurred_at_unix_ms: u64,
    pub action: WebAuthnAuditAction,
    pub correlation_id: Option<DeviceAuthorizationId>,
    pub credential_id_sha256: Option<[u8; 32]>,
    pub previous_counter: Option<u32>,
    pub new_counter: Option<u32>,
    pub user_verified: Option<bool>,
    pub backup_eligible: Option<bool>,
    pub backup_state: Option<bool>,
    pub reason_code: Option<String>,
}

/// SQLite-backed credentials, ceremony states, replay records and audit for
/// local or single-instance use. This is not suitable for ephemeral ACA
/// storage or replicas; [`crate::PostgresWebAuthnCredentialStore`] provides
/// shared production persistence after operators provision its database role
/// and schema and the service injects trusted RP and enrollment policy.
pub struct SqliteWebAuthnCredentialStore {
    connection: Mutex<Connection>,
}

impl SqliteWebAuthnCredentialStore {
    /// Open a private SQLite database in an existing private directory.
    ///
    /// This adapter is suitable only for local or single-instance deployment
    /// with a persistent disk. ACA replicas require a shared database adapter.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WebAuthnCredentialStoreError> {
        let connection = open_private_database(path.as_ref())?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, WebAuthnCredentialStoreError> {
        self.connection
            .lock()
            .map_err(|_| WebAuthnCredentialStoreError::Unavailable)
    }
}

/// Authenticated session boundary used before starting, finishing or revoking
/// a user's credentials. Implementations must bind `owner` to the current SSO
/// principal and enforce the deployment's step-up authentication policy.
pub trait WebAuthnCredentialEnrollmentAuthorizer: Send + Sync {
    fn authorize_credential_management(
        &self,
        owner: &UserIdentityRef,
        action: WebAuthnCredentialManagementAction,
    ) -> Result<(), DeviceAuthorizationPortError>;
}

/// Credential-management operation passed to the authenticated authorizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnCredentialManagementAction {
    Enroll,
    Revoke,
}

/// Public challenge returned by the protected credential-enrollment port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnCredentialRegistrationChallenge {
    pub registration_id: DeviceAuthorizationId,
    pub credential_creation_options_json: Vec<u8>,
}

/// Protected WebAuthn credential enrollment and revocation port.
pub struct WebAuthnCredentialEnrollmentService {
    webauthn: Webauthn,
    config: WebAuthnVerifierConfig,
    store: Arc<dyn WebAuthnCredentialStore>,
    clock: Arc<dyn DeviceAuthorizationClockPort>,
    authorizer: Arc<dyn WebAuthnCredentialEnrollmentAuthorizer>,
}

impl WebAuthnCredentialEnrollmentService {
    /// Require a fixed RP, durable store, trusted clock and SSO authorization.
    pub fn new(
        config: WebAuthnVerifierConfig,
        store: Arc<dyn WebAuthnCredentialStore>,
        clock: Arc<dyn DeviceAuthorizationClockPort>,
        authorizer: Arc<dyn WebAuthnCredentialEnrollmentAuthorizer>,
    ) -> Result<Self, WebAuthnVerifierConfigError> {
        let webauthn = config.build_webauthn()?;
        Ok(Self {
            webauthn,
            config,
            store,
            clock,
            authorizer,
        })
    }

    /// Start registration only after the current SSO session is authorized.
    pub fn start_registration(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<WebAuthnCredentialRegistrationChallenge, DeviceAuthorizationPortError> {
        validate_owner(owner).map_err(|_| DeviceAuthorizationPortError::Rejected)?;
        self.authorizer
            .authorize_credential_management(owner, WebAuthnCredentialManagementAction::Enroll)?;
        let now = self.clock.current_unix_ms()?;
        let expires_at = ceremony_expiry(now, self.config.ceremony_ttl)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let user_handle = self
            .store
            .ensure_user_handle(owner)
            .map_err(store_to_port_error)?;
        let credential_set = self
            .store
            .credentials_for_owner(owner)
            .map_err(store_to_port_error)?
            .unwrap_or_else(|| StoredWebAuthnCredentialSet {
                user_handle: user_handle.clone(),
                credentials: Vec::new(),
            });
        if credential_set.user_handle.ct_eq(&user_handle).unwrap_u8() != 1 {
            return Err(DeviceAuthorizationPortError::Unavailable);
        }
        let uuid = Uuid::from_slice(&user_handle)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let excluded_credentials = credential_set
            .credentials
            .iter()
            .map(|credential| credential.passkey.cred_id().clone())
            .collect();
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                uuid,
                "workspace-user",
                "Workspace user",
                Some(excluded_credentials),
            )
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let options_json =
            serde_json::to_vec(&options).map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let opaque_state =
            serde_json::to_vec(&state).map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        if options_json.is_empty()
            || options_json.len() > MAX_STATE_BYTES
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_STATE_BYTES
        {
            return Err(DeviceAuthorizationPortError::Unavailable);
        }
        let registration_id = Uuid::new_v4().into_bytes();
        self.store
            .begin_registration(
                &registration_id,
                owner,
                &user_handle,
                &opaque_state,
                expires_at,
                now,
            )
            .map_err(store_to_port_error)?;
        Ok(WebAuthnCredentialRegistrationChallenge {
            registration_id,
            credential_creation_options_json: options_json,
        })
    }

    /// Verify an authenticator registration before it can enter the store.
    pub fn finish_registration(
        &self,
        owner: &UserIdentityRef,
        registration_id: &DeviceAuthorizationId,
        response_json: &[u8],
    ) -> Result<(), DeviceAuthorizationPortError> {
        validate_owner(owner).map_err(|_| DeviceAuthorizationPortError::Rejected)?;
        self.authorizer
            .authorize_credential_management(owner, WebAuthnCredentialManagementAction::Enroll)?;
        let now = self.clock.current_unix_ms()?;
        if response_json.is_empty() || response_json.len() > MAX_ASSERTION_BYTES {
            return self.reject_registration(
                owner,
                registration_id,
                None,
                WebAuthnAuditFailure::InvalidRegistration,
                now,
            );
        }
        let response_sha256 = sha256(response_json);
        let ceremony = self
            .store
            .registration_ceremony(registration_id)
            .map_err(store_to_port_error)?;
        let Some(ceremony) = ceremony else {
            return self.reject_registration(
                owner,
                registration_id,
                None,
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        };
        if ceremony.owner.issuer != owner.issuer || ceremony.owner.subject != owner.subject {
            return self.reject_registration(
                owner,
                registration_id,
                None,
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        }
        if let Some(consumed) = ceremony.consumed_response_sha256 {
            if consumed.ct_eq(&response_sha256).unwrap_u8() == 1 {
                return Ok(());
            }
            return self.reject_registration(
                owner,
                registration_id,
                ceremony.consumed_credential_id.as_deref().map(sha256),
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        }
        if now >= ceremony.expires_at_unix_ms {
            return self.reject_registration(
                owner,
                registration_id,
                None,
                WebAuthnAuditFailure::CeremonyExpired,
                now,
            );
        }
        let response: RegisterPublicKeyCredential = match serde_json::from_slice(response_json) {
            Ok(response) => response,
            Err(_) => {
                return self.reject_registration(
                    owner,
                    registration_id,
                    None,
                    WebAuthnAuditFailure::InvalidRegistration,
                    now,
                )
            }
        };
        let state: webauthn_rs::prelude::PasskeyRegistration =
            serde_json::from_slice(&ceremony.opaque_state)
                .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let passkey = match self.webauthn.finish_passkey_registration(&response, &state) {
            Ok(passkey) => passkey,
            Err(_) => {
                return self.reject_registration(
                    owner,
                    registration_id,
                    Some(sha256(response.raw_id.as_ref())),
                    WebAuthnAuditFailure::InvalidRegistration,
                    now,
                )
            }
        };
        let internal = Credential::from(passkey.clone());
        if !internal.user_verified {
            return self.reject_registration(
                owner,
                registration_id,
                Some(sha256(passkey.cred_id().as_ref())),
                WebAuthnAuditFailure::UserVerificationMissing,
                now,
            );
        }
        if self.config.backup_policy == WebAuthnBackupCredentialPolicy::RejectBackupEligible
            && (internal.backup_eligible || internal.backup_state)
        {
            return self.reject_registration(
                owner,
                registration_id,
                Some(sha256(passkey.cred_id().as_ref())),
                WebAuthnAuditFailure::BackupCredentialDisallowed,
                now,
            );
        }
        let verified = VerifiedWebAuthnPasskey {
            owner: owner.clone(),
            user_handle: ceremony.user_handle,
            signature_counter: internal.counter,
            backup_eligible: internal.backup_eligible,
            backup_state: internal.backup_state,
            passkey,
        };
        self.store
            .commit_registration(
                registration_id,
                owner,
                &ceremony.opaque_state,
                response_sha256,
                &verified,
                now,
            )
            .map_err(store_to_port_error)
    }

    /// Revoke a passkey through the same authenticated identity boundary.
    pub fn revoke_credential(
        &self,
        owner: &UserIdentityRef,
        credential_id: &[u8],
        reason: WebAuthnCredentialRevocationReason,
    ) -> Result<bool, DeviceAuthorizationPortError> {
        validate_owner(owner).map_err(|_| DeviceAuthorizationPortError::Rejected)?;
        self.authorizer
            .authorize_credential_management(owner, WebAuthnCredentialManagementAction::Revoke)?;
        let now = self.clock.current_unix_ms()?;
        self.store
            .revoke_credential(owner, credential_id, reason, now)
            .map_err(store_to_port_error)
    }

    fn reject_registration<T>(
        &self,
        owner: &UserIdentityRef,
        registration_id: &DeviceAuthorizationId,
        credential_id_sha256: Option<[u8; 32]>,
        reason: WebAuthnAuditFailure,
        now_unix_ms: u64,
    ) -> Result<T, DeviceAuthorizationPortError> {
        self.store
            .record_rejection(
                owner,
                registration_id,
                credential_id_sha256,
                reason,
                now_unix_ms,
            )
            .map_err(store_to_port_error)?;
        Err(DeviceAuthorizationPortError::Rejected)
    }
}

/// WebAuthn verifier that fixes the RP ID/origin and delegates durable
/// ceremony, credential, replay and audit mutations to a transactional store.
///
/// Credentials with a nonzero signature counter must strictly increase it.
/// Authenticators that always report zero remain usable, with replay
/// prevention provided by the one-time server-side challenge. Backup-eligible
/// credentials are rejected by default; opting in is explicit and audited.
pub struct WebAuthnAuthenticationVerifier {
    webauthn: Webauthn,
    config: WebAuthnVerifierConfig,
    store: Arc<dyn WebAuthnCredentialStore>,
    clock: Arc<dyn DeviceAuthorizationClockPort>,
}

impl WebAuthnAuthenticationVerifier {
    /// Build the production assertion verifier. No credentials means fail-closed
    /// rejection until the protected enrollment service registers one.
    pub fn new(
        config: WebAuthnVerifierConfig,
        store: Arc<dyn WebAuthnCredentialStore>,
        clock: Arc<dyn DeviceAuthorizationClockPort>,
    ) -> Result<Self, WebAuthnVerifierConfigError> {
        let webauthn = config.build_webauthn()?;
        Ok(Self {
            webauthn,
            config,
            store,
            clock,
        })
    }

    fn reject<T>(
        &self,
        context: &WebAuthnAuthenticationContext,
        assertion: &[u8],
        credential_id_sha256: Option<[u8; 32]>,
        reason: WebAuthnAuditFailure,
        now_unix_ms: u64,
    ) -> Result<T, DeviceAuthorizationPortError> {
        self.store
            .record_rejection(
                &context.approver,
                &context.approval_id,
                credential_id_sha256.or_else(|| (!assertion.is_empty()).then(|| sha256(assertion))),
                reason,
                now_unix_ms,
            )
            .map_err(store_to_port_error)?;
        Err(DeviceAuthorizationPortError::Rejected)
    }
}

impl WebAuthnAuthenticationPort for WebAuthnAuthenticationVerifier {
    fn start_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
    ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
        validate_context(context).map_err(|_| DeviceAuthorizationPortError::Rejected)?;
        let now = self.clock.current_unix_ms()?;
        if now >= context.expires_at_unix_ms {
            return Err(DeviceAuthorizationPortError::Rejected);
        }
        let credential_set = self
            .store
            .credentials_for_owner(&context.approver)
            .map_err(store_to_port_error)?;
        let Some(credential_set) = credential_set else {
            return self.reject(
                context,
                &[],
                None,
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        };
        if credential_set.credentials.is_empty() || !valid_user_handle(&credential_set.user_handle)
        {
            return self.reject(
                context,
                &[],
                None,
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        }
        let credentials = credential_set
            .credentials
            .iter()
            .map(|credential| credential.passkey.clone())
            .collect::<Vec<_>>();
        if self.config.backup_policy == WebAuthnBackupCredentialPolicy::RejectBackupEligible {
            if let Some(credential) = credential_set.credentials.iter().find(|credential| {
                let internal = Credential::from(credential.passkey.clone());
                internal.backup_eligible || internal.backup_state
            }) {
                return self.reject(
                    context,
                    &[],
                    Some(sha256(&credential.credential_id)),
                    WebAuthnAuditFailure::BackupCredentialDisallowed,
                    now,
                );
            }
        }
        let credential_set_sha256 = credential_set_digest(&credential_set.credentials);
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&credentials)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let options_json =
            serde_json::to_vec(&options).map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let opaque_state =
            serde_json::to_vec(&state).map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        if options_json.is_empty()
            || options_json.len() > MAX_STATE_BYTES
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_STATE_BYTES
            || !request_options_are_pinned(&options_json, &self.config.rp_id)
        {
            return Err(DeviceAuthorizationPortError::Unavailable);
        }
        let expires_at = ceremony_expiry(now, self.config.ceremony_ttl)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?
            .min(context.expires_at_unix_ms);
        if expires_at <= now {
            return Err(DeviceAuthorizationPortError::Rejected);
        }
        self.store
            .begin_authentication(
                context,
                &credential_set.user_handle,
                &opaque_state,
                credential_set_sha256,
                expires_at,
                now,
            )
            .map_err(store_to_port_error)?;
        Ok(WebAuthnAuthenticationStart {
            credential_request_options_json: options_json,
            opaque_state,
        })
    }

    fn finish_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        opaque_state: &[u8],
        assertion: &[u8],
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationPortError> {
        validate_context(context).map_err(|_| DeviceAuthorizationPortError::Rejected)?;
        if assertion.is_empty()
            || assertion.len() > MAX_ASSERTION_BYTES
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_STATE_BYTES
        {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::InvalidAssertion,
                now_unix_ms,
            );
        }
        let trusted_now = self.clock.current_unix_ms()?;
        let now = trusted_now.max(now_unix_ms);
        if now >= context.expires_at_unix_ms {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::CeremonyExpired,
                now,
            );
        }
        let ceremony = self
            .store
            .authentication_ceremony(&context.approval_id)
            .map_err(store_to_port_error)?;
        let Some(ceremony) = ceremony else {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        };
        let context_sha256 = context_digest(context);
        let state_sha256 = sha256(opaque_state);
        if ceremony.authorization_id != context.authorization_id
            || !same_owner(&ceremony.owner, &context.approver)
            || ceremony.context_sha256.ct_eq(&context_sha256).unwrap_u8() != 1
            || ceremony.state_sha256.ct_eq(&state_sha256).unwrap_u8() != 1
            || ceremony.opaque_state.ct_eq(opaque_state).unwrap_u8() != 1
        {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        }
        let assertion_sha256 = sha256(assertion);
        if let Some(consumed) = ceremony.consumed_assertion_sha256 {
            if consumed.ct_eq(&assertion_sha256).unwrap_u8() == 1 {
                return Ok(());
            }
            return self.reject(
                context,
                assertion,
                ceremony.consumed_credential_id.as_deref().map(sha256),
                WebAuthnAuditFailure::StateOrOwnerMismatch,
                now,
            );
        }
        if now >= ceremony.expires_at_unix_ms {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::CeremonyExpired,
                now,
            );
        }
        let credential_set = self
            .store
            .credentials_for_owner(&context.approver)
            .map_err(store_to_port_error)?;
        let Some(credential_set) = credential_set else {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        };
        if !valid_user_handle(&credential_set.user_handle)
            || credential_set
                .user_handle
                .ct_eq(&ceremony_user_handle(&ceremony))
                .unwrap_u8()
                != 1
            || credential_set_digest(&credential_set.credentials)
                .ct_eq(&ceremony.credential_set_sha256)
                .unwrap_u8()
                != 1
        {
            return self.reject(
                context,
                assertion,
                None,
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        }
        let parsed_assertion: PublicKeyCredential = match serde_json::from_slice(assertion) {
            Ok(assertion) => assertion,
            Err(_) => {
                return self.reject(
                    context,
                    assertion,
                    None,
                    WebAuthnAuditFailure::InvalidAssertion,
                    now,
                )
            }
        };
        if let Some(user_handle) = parsed_assertion.get_user_unique_id() {
            if user_handle.ct_eq(&credential_set.user_handle).unwrap_u8() != 1 {
                return self.reject(
                    context,
                    assertion,
                    Some(sha256(parsed_assertion.get_credential_id())),
                    WebAuthnAuditFailure::UserHandleMismatch,
                    now,
                );
            }
        }
        let state: PasskeyAuthentication = serde_json::from_slice(&ceremony.opaque_state)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        let result = match self
            .webauthn
            .finish_passkey_authentication(&parsed_assertion, &state)
        {
            Ok(result) => result,
            Err(_) => {
                return self.reject(
                    context,
                    assertion,
                    Some(sha256(parsed_assertion.get_credential_id())),
                    WebAuthnAuditFailure::InvalidAssertion,
                    now,
                )
            }
        };
        if !result.user_verified() {
            return self.reject(
                context,
                assertion,
                Some(sha256(result.cred_id().as_ref())),
                WebAuthnAuditFailure::UserVerificationMissing,
                now,
            );
        }
        if self.config.backup_policy == WebAuthnBackupCredentialPolicy::RejectBackupEligible
            && (result.backup_eligible() || result.backup_state())
        {
            return self.reject(
                context,
                assertion,
                Some(sha256(result.cred_id().as_ref())),
                WebAuthnAuditFailure::BackupCredentialDisallowed,
                now,
            );
        }
        let credential_id = result.cred_id().as_ref();
        let Some(stored_credential) = credential_set
            .credentials
            .iter()
            .find(|credential| credential.credential_id.as_slice() == credential_id)
        else {
            return self.reject(
                context,
                assertion,
                Some(sha256(credential_id)),
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        };
        let old_counter = stored_credential.signature_counter;
        let new_counter = result.counter();
        if old_counter > 0 && new_counter <= old_counter {
            return self.reject(
                context,
                assertion,
                Some(sha256(credential_id)),
                WebAuthnAuditFailure::SignatureCounterReplay,
                now,
            );
        }
        let mut updated_passkey = stored_credential.passkey.clone();
        if updated_passkey.update_credential(&result).is_none() {
            return self.reject(
                context,
                assertion,
                Some(sha256(credential_id)),
                WebAuthnAuditFailure::CredentialChangedOrRevoked,
                now,
            );
        }
        self.store
            .commit_authentication(WebAuthnAuthenticationCommit {
                context,
                opaque_state,
                assertion_sha256,
                credential_id,
                expected_counter: old_counter,
                expected_passkey_sha256: stored_credential.serialized_sha256,
                updated_passkey: &updated_passkey,
                now_unix_ms: now,
                user_verified: result.user_verified(),
                backup_eligible: result.backup_eligible(),
                backup_state: result.backup_state(),
            })
            .map_err(store_to_port_error)
    }
}

/// Trusted wall-clock implementation shared by registration and verification.
pub struct WebAuthnVerifierSystemClock;

impl DeviceAuthorizationClockPort for WebAuthnVerifierSystemClock {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)?;
        duration
            .as_millis()
            .try_into()
            .map_err(|_| DeviceAuthorizationPortError::Unavailable)
    }
}

impl WebAuthnCredentialStore for SqliteWebAuthnCredentialStore {
    fn ensure_user_handle(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<Vec<u8>, WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        if let Some(handle) = owner_handle(&transaction, owner)? {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(handle);
        }
        for _ in 0..8 {
            let handle = Uuid::new_v4().into_bytes().to_vec();
            match transaction.execute(
                "INSERT INTO webauthn_owners (issuer, subject, user_handle) VALUES (?1, ?2, ?3)",
                params![owner.issuer, owner.subject, handle],
            ) {
                Ok(_) => {
                    transaction.commit().map_err(sqlite_error)?;
                    return Ok(handle);
                }
                Err(error) if is_unique_constraint(&error) => {
                    if let Some(handle) = owner_handle(&transaction, owner)? {
                        transaction.commit().map_err(sqlite_error)?;
                        return Ok(handle);
                    }
                }
                Err(error) => return Err(sqlite_error(error)),
            }
        }
        Err(WebAuthnCredentialStoreError::Unavailable)
    }

    fn credentials_for_owner(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<Option<StoredWebAuthnCredentialSet>, WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let connection = self.lock()?;
        load_credentials(&connection, owner)
    }

    fn begin_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        user_handle: &[u8],
        opaque_state: &[u8],
        credential_set_sha256: [u8; 32],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError> {
        validate_context(context).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if !valid_user_handle(user_handle)
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_STATE_BYTES
            || expires_at_unix_ms <= now_unix_ms
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current_set) = load_credentials(&transaction, &context.approver)? else {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        };
        if current_set.user_handle.ct_eq(user_handle).unwrap_u8() != 1
            || current_set.credentials.is_empty()
            || credential_set_digest(&current_set.credentials)
                .ct_eq(&credential_set_sha256)
                .unwrap_u8()
                != 1
        {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        if let Some(existing) = load_authentication_ceremony(&transaction, &context.approval_id)? {
            let same = existing.authorization_id == context.authorization_id
                && same_owner(&existing.owner, &context.approver)
                && existing
                    .context_sha256
                    .ct_eq(&context_digest(context))
                    .unwrap_u8()
                    == 1
                && existing
                    .opaque_state
                    .as_slice()
                    .ct_eq(opaque_state)
                    .unwrap_u8()
                    == 1
                && existing
                    .credential_set_sha256
                    .ct_eq(&credential_set_sha256)
                    .unwrap_u8()
                    == 1
                && existing.expires_at_unix_ms == expires_at_unix_ms;
            if same {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        transaction
            .execute(
                "DELETE FROM webauthn_authentication_ceremonies WHERE expires_at_unix_ms <= ?1",
                params![to_sql_time(now_unix_ms)?],
            )
            .map_err(sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO webauthn_authentication_ceremonies \
                 (approval_id, authorization_id, issuer, subject, user_handle, context_sha256, \
                  opaque_state, state_sha256, credential_set_sha256, expires_at_unix_ms, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)",
                params![
                    context.approval_id.as_slice(),
                    context.authorization_id.as_slice(),
                    context.approver.issuer,
                    context.approver.subject,
                    user_handle,
                    context_digest(context).as_slice(),
                    opaque_state,
                    sha256(opaque_state).as_slice(),
                    credential_set_sha256.as_slice(),
                    to_sql_time(expires_at_unix_ms)?,
                ],
            )
            .map_err(map_unique_or_sqlite)?;
        insert_audit(
            &transaction,
            &context.approver,
            now_unix_ms,
            WebAuthnAuditAction::AuthenticationChallengeIssued,
            Some(&context.approval_id),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    fn authentication_ceremony(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<PersistedWebAuthnCeremony>, WebAuthnCredentialStoreError> {
        let connection = self.lock()?;
        load_authentication_ceremony(&connection, approval_id)
    }

    fn commit_authentication(
        &self,
        commit: WebAuthnAuthenticationCommit<'_>,
    ) -> Result<(), WebAuthnCredentialStoreError> {
        let WebAuthnAuthenticationCommit {
            context,
            opaque_state,
            assertion_sha256,
            credential_id,
            expected_counter,
            expected_passkey_sha256,
            updated_passkey,
            now_unix_ms,
            user_verified,
            backup_eligible,
            backup_state,
        } = commit;
        let updated = Credential::from(updated_passkey.clone());
        if !user_verified
            || updated.cred_id.as_ref() != credential_id
            || (expected_counter > 0 && updated.counter <= expected_counter)
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let passkey_json = serde_json::to_vec(updated_passkey)
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let ceremony = load_authentication_ceremony(&transaction, &context.approval_id)?
            .ok_or(WebAuthnCredentialStoreError::CeremonyConflict)?;
        if ceremony.authorization_id != context.authorization_id
            || !same_owner(&ceremony.owner, &context.approver)
            || ceremony
                .context_sha256
                .ct_eq(&context_digest(context))
                .unwrap_u8()
                != 1
            || ceremony
                .state_sha256
                .ct_eq(&sha256(opaque_state))
                .unwrap_u8()
                != 1
            || ceremony
                .opaque_state
                .as_slice()
                .ct_eq(opaque_state)
                .unwrap_u8()
                != 1
        {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        if let Some(consumed) = ceremony.consumed_assertion_sha256 {
            if consumed.ct_eq(&assertion_sha256).unwrap_u8() == 1
                && ceremony.consumed_credential_id.as_deref() == Some(credential_id)
            {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        if now_unix_ms >= ceremony.expires_at_unix_ms {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        let row: Option<(Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT passkey_json, signature_counter FROM webauthn_credentials \
                 WHERE issuer = ?1 AND subject = ?2 AND credential_id = ?3",
                params![
                    context.approver.issuer,
                    context.approver.subject,
                    credential_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((current_passkey_json, current_counter)) = row else {
            return Err(WebAuthnCredentialStoreError::CredentialNotFound);
        };
        let current_counter = from_sql_counter(current_counter)?;
        if current_counter != expected_counter
            || sha256(&current_passkey_json)
                .ct_eq(&expected_passkey_sha256)
                .unwrap_u8()
                != 1
            || (current_counter > 0 && updated.counter <= current_counter)
        {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        let updated_rows = transaction
            .execute(
                "UPDATE webauthn_credentials SET passkey_json = ?1, signature_counter = ?2 \
                 WHERE issuer = ?3 AND subject = ?4 AND credential_id = ?5 \
                   AND signature_counter = ?6 AND passkey_json = ?7",
                params![
                    passkey_json,
                    i64::from(updated.counter),
                    context.approver.issuer,
                    context.approver.subject,
                    credential_id,
                    i64::from(expected_counter),
                    current_passkey_json,
                ],
            )
            .map_err(sqlite_error)?;
        if updated_rows != 1 {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        let consumed_rows = transaction
            .execute(
                "UPDATE webauthn_authentication_ceremonies \
                 SET status = 1, assertion_sha256 = ?1, credential_id = ?2 \
                 WHERE approval_id = ?3 AND status = 0",
                params![
                    assertion_sha256.as_slice(),
                    credential_id,
                    context.approval_id.as_slice(),
                ],
            )
            .map_err(sqlite_error)?;
        if consumed_rows != 1 {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        insert_audit(
            &transaction,
            &context.approver,
            now_unix_ms,
            WebAuthnAuditAction::AssertionAccepted,
            Some(&context.approval_id),
            Some(sha256(credential_id)),
            Some(expected_counter),
            Some(updated.counter),
            Some(user_verified),
            Some(backup_eligible),
            Some(backup_state),
            None,
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    fn begin_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        user_handle: &[u8],
        opaque_state: &[u8],
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if !valid_user_handle(user_handle)
            || opaque_state.is_empty()
            || opaque_state.len() > MAX_STATE_BYTES
            || expires_at_unix_ms <= now_unix_ms
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let stored_handle = owner_handle(&transaction, owner)?
            .ok_or(WebAuthnCredentialStoreError::CredentialNotFound)?;
        if stored_handle.ct_eq(user_handle).unwrap_u8() != 1 {
            return Err(WebAuthnCredentialStoreError::UserHandleConflict);
        }
        transaction
            .execute(
                "DELETE FROM webauthn_registration_ceremonies WHERE expires_at_unix_ms <= ?1",
                params![to_sql_time(now_unix_ms)?],
            )
            .map_err(sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO webauthn_registration_ceremonies \
                 (registration_id, issuer, subject, user_handle, opaque_state, state_sha256, \
                  expires_at_unix_ms, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                params![
                    registration_id.as_slice(),
                    owner.issuer,
                    owner.subject,
                    user_handle,
                    opaque_state,
                    sha256(opaque_state).as_slice(),
                    to_sql_time(expires_at_unix_ms)?,
                ],
            )
            .map_err(map_unique_or_sqlite)?;
        insert_audit(
            &transaction,
            owner,
            now_unix_ms,
            WebAuthnAuditAction::CredentialRegistrationStarted,
            Some(registration_id),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    fn registration_ceremony(
        &self,
        registration_id: &DeviceAuthorizationId,
    ) -> Result<Option<PersistedWebAuthnRegistration>, WebAuthnCredentialStoreError> {
        let connection = self.lock()?;
        load_registration_ceremony(&connection, registration_id)
    }

    fn commit_registration(
        &self,
        registration_id: &DeviceAuthorizationId,
        owner: &UserIdentityRef,
        opaque_state: &[u8],
        response_sha256: [u8; 32],
        credential: &VerifiedWebAuthnPasskey,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError> {
        if !same_owner(&credential.owner, owner)
            || !valid_user_handle(&credential.user_handle)
            || !Credential::from(credential.passkey.clone()).user_verified
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let passkey_json = serde_json::to_vec(&credential.passkey)
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let internal = Credential::from(credential.passkey.clone());
        if internal.counter != credential.signature_counter
            || internal.backup_eligible != credential.backup_eligible
            || internal.backup_state != credential.backup_state
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let credential_id = credential.passkey.cred_id().as_ref();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let ceremony = load_registration_ceremony(&transaction, registration_id)?
            .ok_or(WebAuthnCredentialStoreError::CeremonyConflict)?;
        if !same_owner(&ceremony.owner, owner)
            || ceremony
                .user_handle
                .ct_eq(&credential.user_handle)
                .unwrap_u8()
                != 1
            || ceremony
                .state_sha256
                .ct_eq(&sha256(opaque_state))
                .unwrap_u8()
                != 1
            || ceremony
                .opaque_state
                .as_slice()
                .ct_eq(opaque_state)
                .unwrap_u8()
                != 1
        {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        if let Some(consumed) = ceremony.consumed_response_sha256 {
            if consumed.ct_eq(&response_sha256).unwrap_u8() == 1
                && ceremony.consumed_credential_id.as_deref() == Some(credential_id)
            {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        if now_unix_ms >= ceremony.expires_at_unix_ms {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        let owner_handle = owner_handle(&transaction, owner)?
            .ok_or(WebAuthnCredentialStoreError::CredentialNotFound)?;
        if owner_handle.ct_eq(&credential.user_handle).unwrap_u8() != 1 {
            return Err(WebAuthnCredentialStoreError::UserHandleConflict);
        }
        transaction
            .execute(
                "INSERT INTO webauthn_credentials \
                 (credential_id, issuer, subject, passkey_json, signature_counter) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    credential_id,
                    owner.issuer,
                    owner.subject,
                    passkey_json,
                    i64::from(credential.signature_counter),
                ],
            )
            .map_err(map_unique_or_sqlite)?;
        let consumed_rows = transaction
            .execute(
                "UPDATE webauthn_registration_ceremonies \
                 SET status = 1, response_sha256 = ?1, credential_id = ?2 \
                 WHERE registration_id = ?3 AND status = 0",
                params![
                    response_sha256.as_slice(),
                    credential_id,
                    registration_id.as_slice(),
                ],
            )
            .map_err(sqlite_error)?;
        if consumed_rows != 1 {
            return Err(WebAuthnCredentialStoreError::CeremonyConflict);
        }
        insert_audit(
            &transaction,
            owner,
            now_unix_ms,
            WebAuthnAuditAction::CredentialRegistered,
            Some(registration_id),
            Some(sha256(credential_id)),
            None,
            Some(credential.signature_counter),
            Some(true),
            Some(credential.backup_eligible),
            Some(credential.backup_state),
            None,
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    fn revoke_credential(
        &self,
        owner: &UserIdentityRef,
        credential_id: &[u8],
        reason: WebAuthnCredentialRevocationReason,
        now_unix_ms: u64,
    ) -> Result<bool, WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        if credential_id.is_empty() || credential_id.len() > 1024 {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let row: Option<(Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT passkey_json, signature_counter FROM webauthn_credentials \
                 WHERE issuer = ?1 AND subject = ?2 AND credential_id = ?3",
                params![owner.issuer, owner.subject, credential_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((passkey_json, counter)) = row else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let passkey: Passkey = serde_json::from_slice(&passkey_json)
            .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
        let internal = Credential::from(passkey);
        if from_sql_counter(counter)? != internal.counter {
            return Err(WebAuthnCredentialStoreError::Unavailable);
        }
        transaction
            .execute(
                "DELETE FROM webauthn_credentials WHERE issuer = ?1 AND subject = ?2 AND credential_id = ?3",
                params![owner.issuer, owner.subject, credential_id],
            )
            .map_err(sqlite_error)?;
        insert_audit(
            &transaction,
            owner,
            now_unix_ms,
            WebAuthnAuditAction::CredentialRevoked,
            None,
            Some(sha256(credential_id)),
            Some(internal.counter),
            None,
            None,
            Some(internal.backup_eligible),
            Some(internal.backup_state),
            Some(reason.code()),
        )?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(true)
    }

    fn record_rejection(
        &self,
        owner: &UserIdentityRef,
        correlation_id: &DeviceAuthorizationId,
        credential_id_sha256: Option<[u8; 32]>,
        reason: WebAuthnAuditFailure,
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        insert_audit(
            &transaction,
            owner,
            now_unix_ms,
            WebAuthnAuditAction::SecurityRejected,
            Some(correlation_id),
            credential_id_sha256,
            None,
            None,
            None,
            None,
            None,
            Some(reason.code()),
        )?;
        transaction.commit().map_err(sqlite_error)
    }

    fn audit_events(
        &self,
        owner: &UserIdentityRef,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<WebAuthnAuditEvent>, WebAuthnCredentialStoreError> {
        validate_owner(owner).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let connection = self.lock()?;
        let after_sequence = to_sql_time(after_sequence)?;
        let limit = i64::try_from(limit.min(AUDIT_PAGE_LIMIT))
            .map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, occurred_at_unix_ms, action, correlation_id, \
                 credential_id_sha256, previous_counter, new_counter, user_verified, \
                 backup_eligible, backup_state, reason_code FROM webauthn_audit_events \
                 WHERE issuer = ?1 AND subject = ?2 AND sequence > ?3 \
                 ORDER BY sequence ASC LIMIT ?4",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(
                params![owner.issuer, owner.subject, after_sequence, limit],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<bool>>(7)?,
                        row.get::<_, Option<bool>>(8)?,
                        row.get::<_, Option<bool>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .map_err(sqlite_error)?;
        let mut events = Vec::new();
        for row in rows {
            let (
                sequence,
                occurred_at,
                action,
                correlation_id,
                credential_hash,
                previous_counter,
                new_counter,
                user_verified,
                backup_eligible,
                backup_state,
                reason_code,
            ) = row.map_err(sqlite_error)?;
            events.push(WebAuthnAuditEvent {
                sequence: from_sql_time(sequence)?,
                occurred_at_unix_ms: from_sql_time(occurred_at)?,
                action: WebAuthnAuditAction::parse(&action)
                    .ok_or(WebAuthnCredentialStoreError::Unavailable)?,
                correlation_id: correlation_id.map(|value| decode_id(&value)).transpose()?,
                credential_id_sha256: credential_hash
                    .map(|value| decode_digest(&value))
                    .transpose()?,
                previous_counter: previous_counter.map(from_sql_counter).transpose()?,
                new_counter: new_counter.map(from_sql_counter).transpose()?,
                user_verified,
                backup_eligible,
                backup_state,
                reason_code,
            });
        }
        Ok(events)
    }
}

fn open_private_database(path: &Path) -> Result<Connection, WebAuthnCredentialStoreError> {
    if !path.is_absolute() {
        return Err(WebAuthnCredentialStoreError::InvalidPath);
    }
    let filename = path
        .file_name()
        .ok_or(WebAuthnCredentialStoreError::InvalidPath)?;
    let parent = path
        .parent()
        .ok_or(WebAuthnCredentialStoreError::InvalidPath)?;
    let parent = fs::canonicalize(parent).map_err(|_| WebAuthnCredentialStoreError::InvalidPath)?;
    let path = parent.join(filename);
    verify_private_parent(&parent)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => verify_private_file(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => file
                    .sync_all()
                    .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&path)
                        .map_err(|_| WebAuthnCredentialStoreError::InvalidPath)?;
                    verify_private_file(&metadata)?;
                }
                Err(_) => return Err(WebAuthnCredentialStoreError::InvalidPath),
            }
        }
        Err(_) => return Err(WebAuthnCredentialStoreError::InvalidPath),
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(&path, flags).map_err(sqlite_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(sqlite_error)?;
    Ok(connection)
}

fn verify_private_parent(path: &Path) -> Result<(), WebAuthnCredentialStoreError> {
    if !path.is_dir() {
        return Err(WebAuthnCredentialStoreError::InvalidPath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).map_err(|_| WebAuthnCredentialStoreError::InvalidPath)?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(WebAuthnCredentialStoreError::InsecurePermissions);
        }
    }
    Ok(())
}

fn verify_private_file(metadata: &fs::Metadata) -> Result<(), WebAuthnCredentialStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WebAuthnCredentialStoreError::InvalidPath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(WebAuthnCredentialStoreError::InsecurePermissions);
        }
    }
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<(), WebAuthnCredentialStoreError> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version == 1 {
        return Ok(());
    }
    if version != 0 {
        return Err(WebAuthnCredentialStoreError::Unavailable);
    }
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE webauthn_owners (
                 issuer TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 user_handle BLOB NOT NULL UNIQUE CHECK(length(user_handle) BETWEEN 16 AND 64),
                 PRIMARY KEY (issuer, subject)
             );
             CREATE TABLE webauthn_credentials (
                 credential_id BLOB PRIMARY KEY NOT NULL,
                 issuer TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 passkey_json BLOB NOT NULL,
                 signature_counter INTEGER NOT NULL CHECK(signature_counter BETWEEN 0 AND 4294967295),
                 FOREIGN KEY (issuer, subject) REFERENCES webauthn_owners(issuer, subject)
             );
             CREATE INDEX webauthn_credentials_owner_idx
                 ON webauthn_credentials(issuer, subject, credential_id);
             CREATE TABLE webauthn_authentication_ceremonies (
                 approval_id BLOB PRIMARY KEY NOT NULL CHECK(length(approval_id) = 16),
                 authorization_id BLOB NOT NULL CHECK(length(authorization_id) = 16),
                 issuer TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 user_handle BLOB NOT NULL,
                 context_sha256 BLOB NOT NULL CHECK(length(context_sha256) = 32),
                 opaque_state BLOB NOT NULL,
                 state_sha256 BLOB NOT NULL CHECK(length(state_sha256) = 32),
                 credential_set_sha256 BLOB NOT NULL CHECK(length(credential_set_sha256) = 32),
                 expires_at_unix_ms INTEGER NOT NULL,
                 status INTEGER NOT NULL CHECK(status IN (0, 1)),
                 assertion_sha256 BLOB CHECK(assertion_sha256 IS NULL OR length(assertion_sha256) = 32),
                 credential_id BLOB,
                 FOREIGN KEY (issuer, subject) REFERENCES webauthn_owners(issuer, subject),
                 CHECK ((status = 0 AND assertion_sha256 IS NULL AND credential_id IS NULL) OR
                        (status = 1 AND assertion_sha256 IS NOT NULL AND credential_id IS NOT NULL))
             );
             CREATE TABLE webauthn_registration_ceremonies (
                 registration_id BLOB PRIMARY KEY NOT NULL CHECK(length(registration_id) = 16),
                 issuer TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 user_handle BLOB NOT NULL,
                 opaque_state BLOB NOT NULL,
                 state_sha256 BLOB NOT NULL CHECK(length(state_sha256) = 32),
                 expires_at_unix_ms INTEGER NOT NULL,
                 status INTEGER NOT NULL CHECK(status IN (0, 1)),
                 response_sha256 BLOB CHECK(response_sha256 IS NULL OR length(response_sha256) = 32),
                 credential_id BLOB,
                 FOREIGN KEY (issuer, subject) REFERENCES webauthn_owners(issuer, subject),
                 CHECK ((status = 0 AND response_sha256 IS NULL AND credential_id IS NULL) OR
                        (status = 1 AND response_sha256 IS NOT NULL AND credential_id IS NOT NULL))
             );
             CREATE TABLE webauthn_audit_events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 occurred_at_unix_ms INTEGER NOT NULL,
                 issuer TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 action TEXT NOT NULL,
                 correlation_id BLOB CHECK(correlation_id IS NULL OR length(correlation_id) = 16),
                 credential_id_sha256 BLOB CHECK(credential_id_sha256 IS NULL OR length(credential_id_sha256) = 32),
                 previous_counter INTEGER,
                 new_counter INTEGER,
                 user_verified INTEGER,
                 backup_eligible INTEGER,
                 backup_state INTEGER,
                 reason_code TEXT
             );
             CREATE INDEX webauthn_audit_owner_idx
                 ON webauthn_audit_events(issuer, subject, sequence);
             CREATE TRIGGER webauthn_audit_no_update
                 BEFORE UPDATE ON webauthn_audit_events
                 BEGIN SELECT RAISE(ABORT, 'audit events are append-only'); END;
             CREATE TRIGGER webauthn_audit_no_delete
                 BEFORE DELETE ON webauthn_audit_events
                 BEGIN SELECT RAISE(ABORT, 'audit events are append-only'); END;
             PRAGMA user_version = 1;
             COMMIT;",
        )
        .map_err(sqlite_error)
}

fn load_credentials(
    connection: &Connection,
    owner: &UserIdentityRef,
) -> Result<Option<StoredWebAuthnCredentialSet>, WebAuthnCredentialStoreError> {
    let Some(user_handle) = owner_handle(connection, owner)? else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT credential_id, passkey_json, signature_counter FROM webauthn_credentials \
             WHERE issuer = ?1 AND subject = ?2 ORDER BY credential_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![owner.issuer, owner.subject], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut credentials = Vec::new();
    for row in rows {
        let (credential_id, passkey_json, counter) = row.map_err(sqlite_error)?;
        let passkey: Passkey = serde_json::from_slice(&passkey_json)
            .map_err(|_| WebAuthnCredentialStoreError::Unavailable)?;
        let internal = Credential::from(passkey.clone());
        let counter = from_sql_counter(counter)?;
        if credential_id.as_slice() != passkey.cred_id().as_ref() || counter != internal.counter {
            return Err(WebAuthnCredentialStoreError::Unavailable);
        }
        credentials.push(StoredWebAuthnCredential {
            credential_id,
            passkey,
            signature_counter: counter,
            serialized_sha256: sha256(&passkey_json),
        });
    }
    Ok(Some(StoredWebAuthnCredentialSet {
        user_handle,
        credentials,
    }))
}

fn owner_handle(
    connection: &Connection,
    owner: &UserIdentityRef,
) -> Result<Option<Vec<u8>>, WebAuthnCredentialStoreError> {
    connection
        .query_row(
            "SELECT user_handle FROM webauthn_owners WHERE issuer = ?1 AND subject = ?2",
            params![owner.issuer, owner.subject],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)
}

fn load_authentication_ceremony(
    connection: &Connection,
    approval_id: &DeviceAuthorizationId,
) -> Result<Option<PersistedWebAuthnCeremony>, WebAuthnCredentialStoreError> {
    let row = connection
        .query_row(
            "SELECT authorization_id, issuer, subject, user_handle, context_sha256, opaque_state, \
             state_sha256, credential_set_sha256, expires_at_unix_ms, assertion_sha256, credential_id \
             FROM webauthn_authentication_ceremonies WHERE approval_id = ?1",
            params![approval_id.as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                    row.get::<_, Option<Vec<u8>>>(10)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(
        |(
            authorization_id,
            issuer,
            subject,
            user_handle,
            context_sha256,
            opaque_state,
            state_sha256,
            credential_set_sha256,
            expires_at_unix_ms,
            assertion_sha256,
            credential_id,
        )| {
            Ok(PersistedWebAuthnCeremony {
                owner: UserIdentityRef { issuer, subject },
                authorization_id: decode_id(&authorization_id)?,
                user_handle,
                context_sha256: decode_digest(&context_sha256)?,
                state_sha256: decode_digest(&state_sha256)?,
                credential_set_sha256: decode_digest(&credential_set_sha256)?,
                opaque_state,
                expires_at_unix_ms: from_sql_time(expires_at_unix_ms)?,
                consumed_assertion_sha256: assertion_sha256
                    .map(|value| decode_digest(&value))
                    .transpose()?,
                consumed_credential_id: credential_id,
            })
        },
    )
    .transpose()
}

fn load_registration_ceremony(
    connection: &Connection,
    registration_id: &DeviceAuthorizationId,
) -> Result<Option<PersistedWebAuthnRegistration>, WebAuthnCredentialStoreError> {
    let row = connection
        .query_row(
            "SELECT issuer, subject, user_handle, opaque_state, state_sha256, expires_at_unix_ms, \
             response_sha256, credential_id FROM webauthn_registration_ceremonies \
             WHERE registration_id = ?1",
            params![registration_id.as_slice()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(
        |(
            issuer,
            subject,
            user_handle,
            opaque_state,
            state_sha256,
            expires_at_unix_ms,
            response_sha256,
            credential_id,
        )| {
            Ok(PersistedWebAuthnRegistration {
                owner: UserIdentityRef { issuer, subject },
                user_handle,
                opaque_state,
                state_sha256: decode_digest(&state_sha256)?,
                expires_at_unix_ms: from_sql_time(expires_at_unix_ms)?,
                consumed_response_sha256: response_sha256
                    .map(|value| decode_digest(&value))
                    .transpose()?,
                consumed_credential_id: credential_id,
            })
        },
    )
    .transpose()
}

#[allow(clippy::too_many_arguments)]
fn insert_audit(
    connection: &Connection,
    owner: &UserIdentityRef,
    occurred_at_unix_ms: u64,
    action: WebAuthnAuditAction,
    correlation_id: Option<&DeviceAuthorizationId>,
    credential_id_sha256: Option<[u8; 32]>,
    previous_counter: Option<u32>,
    new_counter: Option<u32>,
    user_verified: Option<bool>,
    backup_eligible: Option<bool>,
    backup_state: Option<bool>,
    reason_code: Option<&str>,
) -> Result<(), WebAuthnCredentialStoreError> {
    connection
        .execute(
            "INSERT INTO webauthn_audit_events \
             (occurred_at_unix_ms, issuer, subject, action, correlation_id, credential_id_sha256, \
              previous_counter, new_counter, user_verified, backup_eligible, backup_state, reason_code) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                to_sql_time(occurred_at_unix_ms)?,
                owner.issuer,
                owner.subject,
                action.code(),
                correlation_id.map(|id| id.as_slice()),
                credential_id_sha256.map(|digest| digest.to_vec()),
                previous_counter.map(i64::from),
                new_counter.map(i64::from),
                user_verified,
                backup_eligible,
                backup_state,
                reason_code,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

pub(crate) fn validate_context(
    context: &WebAuthnAuthenticationContext,
) -> Result<(), WebAuthnCredentialStoreError> {
    validate_owner(&context.approver)?;
    if context.expires_at_unix_ms == 0
        || context.scope.organization_id.trim().is_empty()
        || context.scope.workspace_id.trim().is_empty()
        || context.scope.organization_id.len() > MAX_IDENTITY_FIELD_BYTES
        || context.scope.workspace_id.len() > MAX_IDENTITY_FIELD_BYTES
        || context.scope.organization_id.chars().any(char::is_control)
        || context.scope.workspace_id.chars().any(char::is_control)
    {
        return Err(WebAuthnCredentialStoreError::InvalidRecord);
    }
    Ok(())
}

pub(crate) fn validate_owner(owner: &UserIdentityRef) -> Result<(), WebAuthnCredentialStoreError> {
    for value in [&owner.issuer, &owner.subject] {
        if value.trim().is_empty()
            || value.len() > MAX_IDENTITY_FIELD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(WebAuthnCredentialStoreError::InvalidRecord);
        }
    }
    Ok(())
}

pub(crate) fn valid_user_handle(user_handle: &[u8]) -> bool {
    (MIN_USER_HANDLE_BYTES..=MAX_USER_HANDLE_BYTES).contains(&user_handle.len())
}

pub(crate) fn same_owner(left: &UserIdentityRef, right: &UserIdentityRef) -> bool {
    left.issuer == right.issuer && left.subject == right.subject
}

fn ceremony_user_handle(ceremony: &PersistedWebAuthnCeremony) -> Vec<u8> {
    ceremony.user_handle.clone()
}

pub(crate) fn context_digest(context: &WebAuthnAuthenticationContext) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"cyrene.workspace.webauthn.context.v1\0");
    digest.update(context.approval_id);
    digest.update(context.authorization_id);
    hash_field(&mut digest, context.approver.issuer.as_bytes());
    hash_field(&mut digest, context.approver.subject.as_bytes());
    hash_field(&mut digest, context.scope.organization_id.as_bytes());
    hash_field(&mut digest, context.scope.workspace_id.as_bytes());
    digest.update(context.csr_sha256);
    digest.update(context.spki_sha256);
    digest.update(context.expires_at_unix_ms.to_be_bytes());
    digest.finalize().into()
}

pub(crate) fn credential_set_digest(credentials: &[StoredWebAuthnCredential]) -> [u8; 32] {
    let mut credentials = credentials.iter().collect::<Vec<_>>();
    credentials.sort_by(|left, right| left.credential_id.cmp(&right.credential_id));
    let mut digest = Sha256::new();
    digest.update(b"cyrene.workspace.webauthn.credential-set.v1\0");
    digest.update((credentials.len() as u64).to_be_bytes());
    for credential in credentials {
        hash_field(&mut digest, &credential.credential_id);
        digest.update(credential.signature_counter.to_be_bytes());
        digest.update(credential.serialized_sha256);
    }
    digest.finalize().into()
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn request_options_are_pinned(json: &[u8], rp_id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(json) else {
        return false;
    };
    value
        .pointer("/publicKey/rpId")
        .and_then(serde_json::Value::as_str)
        == Some(rp_id)
        && value
            .pointer("/publicKey/userVerification")
            .and_then(serde_json::Value::as_str)
            == Some("required")
}

fn ceremony_expiry(now: u64, ttl: Duration) -> Result<u64, WebAuthnCredentialStoreError> {
    let ttl_ms =
        u64::try_from(ttl.as_millis()).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)?;
    now.checked_add(ttl_ms)
        .ok_or(WebAuthnCredentialStoreError::InvalidRecord)
}

fn sha256(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn decode_id(value: &[u8]) -> Result<DeviceAuthorizationId, WebAuthnCredentialStoreError> {
    value
        .try_into()
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn decode_digest(value: &[u8]) -> Result<[u8; 32], WebAuthnCredentialStoreError> {
    value
        .try_into()
        .map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn to_sql_time(value: u64) -> Result<i64, WebAuthnCredentialStoreError> {
    i64::try_from(value).map_err(|_| WebAuthnCredentialStoreError::InvalidRecord)
}

fn from_sql_time(value: i64) -> Result<u64, WebAuthnCredentialStoreError> {
    u64::try_from(value).map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn from_sql_counter(value: i64) -> Result<u32, WebAuthnCredentialStoreError> {
    u32::try_from(value).map_err(|_| WebAuthnCredentialStoreError::Unavailable)
}

fn sqlite_error(_: rusqlite::Error) -> WebAuthnCredentialStoreError {
    WebAuthnCredentialStoreError::Unavailable
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}

fn map_unique_or_sqlite(error: rusqlite::Error) -> WebAuthnCredentialStoreError {
    if is_unique_constraint(&error) {
        WebAuthnCredentialStoreError::CredentialAlreadyRegistered
    } else {
        sqlite_error(error)
    }
}

fn store_to_port_error(error: WebAuthnCredentialStoreError) -> DeviceAuthorizationPortError {
    match error {
        WebAuthnCredentialStoreError::Unavailable => DeviceAuthorizationPortError::Unavailable,
        _ => DeviceAuthorizationPortError::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use tempfile::TempDir;
    use webauthn_authenticator_rs::prelude::{
        CreationChallengeResponse, RequestChallengeResponse, Url, WebauthnAuthenticator,
    };
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;

    use super::*;
    use crate::device_authorization::{DeviceAuthorizationClockPort, DeviceAuthorizationScope};

    struct FixedClock(AtomicU64);

    impl FixedClock {
        fn new(now_unix_ms: u64) -> Self {
            Self(AtomicU64::new(now_unix_ms))
        }
    }

    impl DeviceAuthorizationClockPort for FixedClock {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }

    struct AllowCredentialManagement;

    impl WebAuthnCredentialEnrollmentAuthorizer for AllowCredentialManagement {
        fn authorize_credential_management(
            &self,
            _owner: &UserIdentityRef,
            _action: WebAuthnCredentialManagementAction,
        ) -> Result<(), DeviceAuthorizationPortError> {
            Ok(())
        }
    }

    fn owner() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://identity.example.test/".to_owned(),
            subject: "workspace-user-7".to_owned(),
        }
    }

    fn authorization_context(owner: UserIdentityRef) -> WebAuthnAuthenticationContext {
        WebAuthnAuthenticationContext {
            approval_id: [0x11; 16],
            authorization_id: [0x22; 16],
            registration_binding_id: [0x55; 16],
            device_key: DeviceAuthorizationDeviceKey {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
                device_id: "device-test".to_owned(),
            },
            authorization_generation: 1,
            approver: owner,
            scope: DeviceAuthorizationScope {
                organization_id: "org-test".to_owned(),
                workspace_id: "workspace-test".to_owned(),
            },
            csr_sha256: [0x33; 32],
            spki_sha256: [0x44; 32],
            expires_at_unix_ms: 1_000_000,
        }
    }

    fn config() -> WebAuthnVerifierConfig {
        WebAuthnVerifierConfig::new(
            "localhost",
            Url::parse("https://localhost/").expect("fixed test origin"),
        )
        .expect("fixed RP config")
    }

    fn tamper_assertion_origin(response: &PublicKeyCredential) -> Vec<u8> {
        let mut value = serde_json::to_value(response).expect("serialize assertion");
        let encoded = value["response"]["clientDataJSON"]
            .as_str()
            .expect("client data");
        let decoded = URL_SAFE_NO_PAD.decode(encoded).expect("decode client data");
        let client_data = String::from_utf8(decoded).expect("UTF-8 client data");
        let tampered = client_data.replace("https://localhost", "https://attacker.example");
        assert_ne!(tampered, client_data, "the origin must be present");
        value["response"]["clientDataJSON"] =
            serde_json::Value::String(URL_SAFE_NO_PAD.encode(tampered.as_bytes()));
        serde_json::to_vec(&value).expect("serialize tampered assertion")
    }

    #[test]
    fn registration_assertion_replay_and_audit_survive_store_reopen() {
        let directory = TempDir::new().expect("private temp directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .expect("restrict temp directory permissions");
        }
        let store_path = directory.path().join("webauthn.sqlite3");
        let owner = owner();
        let clock: Arc<dyn DeviceAuthorizationClockPort> = Arc::new(FixedClock::new(100_000));
        let sqlite_store = Arc::new(
            SqliteWebAuthnCredentialStore::open(&store_path).expect("open credential store"),
        );
        let store: Arc<dyn WebAuthnCredentialStore> = sqlite_store.clone();
        let registrar = WebAuthnCredentialEnrollmentService::new(
            config(),
            store.clone(),
            clock.clone(),
            Arc::new(AllowCredentialManagement),
        )
        .expect("create protected registrar");
        let verifier = WebAuthnAuthenticationVerifier::new(config(), store.clone(), clock.clone())
            .expect("create assertion verifier");
        let context = authorization_context(owner.clone());

        // No credential is inferred or imported: approval is rejected until
        // the protected registration ceremony completes.
        assert_eq!(
            verifier.start_authentication(&context),
            Err(DeviceAuthorizationPortError::Rejected)
        );

        let registration = registrar
            .start_registration(&owner)
            .expect("start authorized enrollment");
        let registration_json: serde_json::Value =
            serde_json::from_slice(&registration.credential_creation_options_json)
                .expect("creation options JSON");
        assert_eq!(
            registration_json
                .pointer("/publicKey/authenticatorSelection/userVerification")
                .and_then(serde_json::Value::as_str),
            Some("required")
        );
        let registration_options: CreationChallengeResponse =
            serde_json::from_slice(&registration.credential_creation_options_json)
                .expect("parse creation options");

        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let registration_response = authenticator
            .do_registration(config().rp_origin().clone(), registration_options)
            .expect("soft authenticator completes registration");
        let registration_response_json =
            serde_json::to_vec(&registration_response).expect("serialize registration response");
        registrar
            .finish_registration(
                &owner,
                &registration.registration_id,
                &registration_response_json,
            )
            .expect("server verifies and stores the registered passkey");

        let registered = store
            .credentials_for_owner(&owner)
            .expect("read credential store")
            .expect("registration created owner record");
        assert_eq!(registered.credentials.len(), 1);
        assert_eq!(registered.credentials[0].signature_counter, 0);
        assert!(!Credential::from(registered.credentials[0].passkey.clone()).backup_eligible);

        // Simulate process restart before finishing an assertion. Credential,
        // challenge state and ceremony status must come from persistent storage.
        drop(verifier);
        drop(registrar);
        drop(store);
        drop(sqlite_store);
        let sqlite_store = Arc::new(
            SqliteWebAuthnCredentialStore::open(&store_path).expect("reopen credential store"),
        );
        let store: Arc<dyn WebAuthnCredentialStore> = sqlite_store.clone();
        let verifier = WebAuthnAuthenticationVerifier::new(config(), store.clone(), clock.clone())
            .expect("recreate verifier after restart");
        let started = verifier
            .start_authentication(&context)
            .expect("start assertion ceremony");
        let request_json: serde_json::Value =
            serde_json::from_slice(&started.credential_request_options_json)
                .expect("assertion options JSON");
        assert_eq!(
            request_json
                .pointer("/publicKey/userVerification")
                .and_then(serde_json::Value::as_str),
            Some("required")
        );
        let request_options: RequestChallengeResponse =
            serde_json::from_slice(&started.credential_request_options_json)
                .expect("parse assertion options");
        let assertion = authenticator
            .do_authentication(config().rp_origin().clone(), request_options)
            .expect("soft authenticator completes assertion");
        let assertion_json = serde_json::to_vec(&assertion).expect("serialize assertion");
        let impostor_context = WebAuthnAuthenticationContext {
            approver: UserIdentityRef {
                issuer: owner.issuer.clone(),
                subject: "different-owner".to_owned(),
            },
            ..context.clone()
        };
        assert_eq!(
            verifier.finish_authentication(
                &impostor_context,
                &started.opaque_state,
                &assertion_json,
                100_000,
            ),
            Err(DeviceAuthorizationPortError::Rejected)
        );
        verifier
            .finish_authentication(&context, &started.opaque_state, &assertion_json, 100_000)
            .expect("server verifies signed, UV assertion and commits counter");
        verifier
            .finish_authentication(&context, &started.opaque_state, &assertion_json, 100_000)
            .expect("same ceremony and assertion are idempotent");

        let accepted = store
            .credentials_for_owner(&owner)
            .expect("read advanced credential")
            .expect("owner record remains");
        assert_eq!(accepted.credentials[0].signature_counter, 1);
        let audit = store
            .audit_events(&owner, 0, AUDIT_PAGE_LIMIT)
            .expect("read audit");
        let impostor = UserIdentityRef {
            issuer: owner.issuer.clone(),
            subject: "different-owner".to_owned(),
        };
        let ownership_audit = store
            .audit_events(&impostor, 0, AUDIT_PAGE_LIMIT)
            .expect("read owner-mismatch audit");
        assert!(ownership_audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::SecurityRejected
                && event.reason_code.as_deref() == Some("STATE_OR_OWNER_MISMATCH")
                && event.correlation_id == Some(context.approval_id)
        }));
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::AssertionAccepted
                && event.previous_counter == Some(0)
                && event.new_counter == Some(1)
                && event.user_verified == Some(true)
                && event.backup_eligible == Some(false)
        }));
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::CredentialRegistered
                && event.user_verified == Some(true)
                && event.backup_eligible == Some(false)
        }));

        // A write-side database failure cannot be converted into an approval.
        sqlite_store
            .connection
            .lock()
            .expect("lock SQLite connection")
            .execute_batch("PRAGMA query_only = ON;")
            .expect("make test store read-only");
        let storage_failure_context = WebAuthnAuthenticationContext {
            approval_id: [0x66; 16],
            ..context.clone()
        };
        assert_eq!(
            verifier.start_authentication(&storage_failure_context),
            Err(DeviceAuthorizationPortError::Unavailable)
        );
        sqlite_store
            .connection
            .lock()
            .expect("lock SQLite connection")
            .execute_batch("PRAGMA query_only = OFF;")
            .expect("restore writable test store");

        // The fixed origin is checked by the WebAuthn library against signed
        // clientDataJSON. Changing it invalidates the assertion signature.
        let second_context = WebAuthnAuthenticationContext {
            approval_id: [0x55; 16],
            ..context.clone()
        };
        let second = verifier
            .start_authentication(&second_context)
            .expect("start second ceremony");
        let request_options: RequestChallengeResponse =
            serde_json::from_slice(&second.credential_request_options_json)
                .expect("parse second assertion options");
        let assertion = authenticator
            .do_authentication(config().rp_origin().clone(), request_options)
            .expect("generate second assertion");
        let tampered_json = tamper_assertion_origin(&assertion);
        assert_eq!(
            verifier.finish_authentication(
                &second_context,
                &second.opaque_state,
                &tampered_json,
                100_000,
            ),
            Err(DeviceAuthorizationPortError::Rejected)
        );
        let audit = store
            .audit_events(&owner, 0, AUDIT_PAGE_LIMIT)
            .expect("read rejection audit");
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::SecurityRejected
                && event.reason_code.as_deref() == Some("INVALID_ASSERTION")
                && event.credential_id_sha256.is_some()
        }));

        let registrar = WebAuthnCredentialEnrollmentService::new(
            config(),
            store.clone(),
            clock,
            Arc::new(AllowCredentialManagement),
        )
        .expect("create revocation service");
        assert!(registrar
            .revoke_credential(
                &owner,
                &accepted.credentials[0].credential_id,
                WebAuthnCredentialRevocationReason::SuspectedCloning,
            )
            .expect("authorized revocation"));
        assert!(store
            .credentials_for_owner(&owner)
            .expect("read revoked credentials")
            .expect("owner record remains")
            .credentials
            .is_empty());
        let audit = store
            .audit_events(&owner, 0, AUDIT_PAGE_LIMIT)
            .expect("read final audit");
        assert!(audit.iter().any(|event| {
            event.action == WebAuthnAuditAction::CredentialRevoked
                && event.reason_code.as_deref() == Some("SUSPECTED_CLONING")
        }));
    }

    #[test]
    fn relying_party_configuration_requires_a_matching_https_origin() {
        assert!(WebAuthnVerifierConfig::new(
            "localhost",
            Url::parse("http://localhost/").expect("test origin"),
        )
        .is_err());
        assert!(WebAuthnVerifierConfig::new(
            "localhost",
            Url::parse("https://attacker.example/").expect("test origin"),
        )
        .is_err());
    }
}
