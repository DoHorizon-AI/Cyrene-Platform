//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_authorization.rs                                         │
//! │  Module: cy_workspace_fabric::device_authorization                  │
//! │  Role: RFC 8628-style device enrollment state machine and ports.    │
//! │                                                                     │
//! │  模块职责：设备授权状态机及可替换的身份、CSR、存储和 CA 端口。           │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This module is a foundation layer, not a production identity or CA
//! implementation. Deployments must supply durable storage, membership,
//! WebAuthn, CSR validation, and certificate-signing ports.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cy_proto::workspace_v1::UserIdentityRef;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{UserCodeKeyRing, UserCodeSecretError, VersionedUserCodeDigest};

/// Opaque server-generated identifier for one authorization attempt.
///
/// This is the wire `authorization_id`; it is not a stable WorkspaceDevice ID.
pub type DeviceAuthorizationId = [u8; 16];

/// Opaque digest stored in place of the high-entropy one-time device code.
pub type DeviceAuthorizationCodeHash = [u8; 32];

/// Exact organization and workspace scope authorized for an enrolled device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationScope {
    pub organization_id: String,
    pub workspace_id: String,
}

/// Input used to start one device enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationRequest {
    pub scope: DeviceAuthorizationScope,
    /// DER-encoded PKCS#10 CSR. Private key material must never be sent here.
    pub csr_der: Vec<u8>,
}

/// Codes and timing returned once to the device and its operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationStart {
    /// 256-bit, server-generated secret; callers must not log or persist it.
    pub device_code: String,
    /// Human-entered code. It is returned once and stored only as a digest.
    pub user_code: String,
    pub expires_at_unix_ms: u64,
    pub poll_interval_ms: u64,
}

/// User-facing request options for one persisted WebAuthn approval attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceApprovalChallenge {
    pub approval_id: DeviceAuthorizationId,
    /// Serialized `PublicKeyCredentialRequestOptions` produced by the WebAuthn port.
    pub credential_request_options_json: Vec<u8>,
}

/// Immutable context binding one WebAuthn ceremony to its authorization.
#[derive(Debug, Clone, PartialEq)]
pub struct WebAuthnAuthenticationContext {
    pub approval_id: DeviceAuthorizationId,
    pub authorization_id: DeviceAuthorizationId,
    pub approver: UserIdentityRef,
    pub scope: DeviceAuthorizationScope,
    pub csr_sha256: [u8; 32],
    pub spki_sha256: [u8; 32],
    pub expires_at_unix_ms: u64,
}

/// Public request options and confidential server state returned by verifier start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthnAuthenticationStart {
    /// JSON bytes to return to the authenticated approver.
    pub credential_request_options_json: Vec<u8>,
    /// Opaque library state. Persist server-side; never return or log it.
    pub opaque_state: Vec<u8>,
}

/// Certificate metadata returned by the configured CA signer.
///
/// The signer adapter must parse and attest that the certificate it produced
/// contains this exact scope and SPKI digest. This struct is not itself proof
/// that a certificate was signed by a trusted CA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedDeviceCertificate {
    pub certificate_der: Vec<u8>,
    pub serial_number: Vec<u8>,
    pub scope: DeviceAuthorizationScope,
    pub spki_sha256: [u8; 32],
    pub not_after_unix_ms: u64,
}

/// Terminal or intermediate state retained for replay prevention and audit.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceAuthorizationState {
    Pending,
    AwaitingWebAuthn {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        credential_request_options_json: Vec<u8>,
        opaque_state: Vec<u8>,
    },
    /// One assertion is reserved while the verifier checks and updates its
    /// credential counter. Denial and expiry may still win before `Issuing`.
    VerifyingWebAuthn {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        assertion_sha256: [u8; 32],
        opaque_state: Vec<u8>,
    },
    /// Durable reservation written before the certificate issuer is called.
    /// Retries must use the enrollment ID as an issuer idempotency key.
    Issuing {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        issued_at_unix_ms: u64,
    },
    Approved {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        decided_at_unix_ms: u64,
        certificate: IssuedDeviceCertificate,
    },
    Denied {
        approver: UserIdentityRef,
        decided_at_unix_ms: u64,
    },
    Consumed {
        approver: UserIdentityRef,
        decided_at_unix_ms: u64,
        consumed_at_unix_ms: u64,
    },
    Expired,
}

/// Persistable authorization record. It contains code digests, never raw codes.
/// The user-code digest includes the HMAC key version used for this record.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizationRecord {
    pub id: DeviceAuthorizationId,
    pub device_code_hash: DeviceAuthorizationCodeHash,
    pub user_code_digest: VersionedUserCodeDigest,
    pub scope: DeviceAuthorizationScope,
    pub csr_der: Vec<u8>,
    pub csr_sha256: [u8; 32],
    pub spki_sha256: [u8; 32],
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub poll_interval_ms: u64,
    pub last_poll_at_unix_ms: Option<u64>,
    /// Monotonic compare-and-swap version for concurrent state transitions.
    pub revision: u64,
    pub state: DeviceAuthorizationState,
}

/// Poll result for the device holding the one-time device code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthorizationPoll {
    Pending {
        interval_ms: u64,
    },
    SlowDown {
        interval_ms: u64,
        retry_after_ms: u64,
    },
    Approved(IssuedDeviceCertificate),
}

/// Tunable limits for short-lived device authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationPolicy {
    pub authorization_ttl_ms: u64,
    pub initial_poll_interval_ms: u64,
    pub maximum_poll_interval_ms: u64,
    pub slow_down_increment_ms: u64,
    pub user_code_attempt_window_ms: u64,
    pub maximum_user_code_attempts: u32,
}

impl Default for DeviceAuthorizationPolicy {
    fn default() -> Self {
        Self {
            authorization_ttl_ms: 10 * 60 * 1_000,
            initial_poll_interval_ms: 5_000,
            maximum_poll_interval_ms: 60_000,
            slow_down_increment_ms: 5_000,
            user_code_attempt_window_ms: 15 * 60 * 1_000,
            maximum_user_code_attempts: 5,
        }
    }
}

impl DeviceAuthorizationPolicy {
    fn is_valid(&self) -> bool {
        self.authorization_ttl_ms > 0
            && self.authorization_ttl_ms <= 10 * 60 * 1_000
            && self.initial_poll_interval_ms > 0
            && self.initial_poll_interval_ms <= self.maximum_poll_interval_ms
            && self.maximum_poll_interval_ms <= 60 * 1_000
            && self.slow_down_increment_ms > 0
            && self.user_code_attempt_window_ms > 0
            && self.maximum_user_code_attempts > 0
            && self.maximum_user_code_attempts <= 10
    }
}

/// Atomic persistence boundary required by a production service.
///
/// Implementations must enforce unique code digests and compare-and-swap
/// revisions across processes. The in-memory implementation below is only for
/// local validation and single-process development.
pub trait DeviceAuthorizationStore: Send + Sync {
    fn insert(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError>;
    fn by_device_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    fn by_user_code_candidates(
        &self,
        candidates: &[VersionedUserCodeDigest],
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    /// Returns every key version still referenced by a stored record.
    ///
    /// Startup must pass these versions to [`UserCodeKeyRing::require_versions`]
    /// before serving user-code lookups.
    fn user_code_key_versions(&self) -> Result<Vec<u32>, DeviceAuthorizationStoreError>;
    /// Resolves the same ID while it is awaiting assertion, issuing, or
    /// approved so an interrupted issuer operation can be resumed safely.
    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError>;
}

/// Storage failures exposed by the authorization state machine.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeviceAuthorizationStoreError {
    #[error("device authorization code collision")]
    CodeCollision,
    #[error("device authorization changed concurrently")]
    Conflict,
    #[error("device authorization storage unavailable")]
    Unavailable,
}

/// Single-process store for tests and local development only.
#[derive(Debug, Clone, Default)]
pub struct InMemoryDeviceAuthorizationStore {
    records: Arc<Mutex<BTreeMap<DeviceAuthorizationId, DeviceAuthorizationRecord>>>,
}

impl DeviceAuthorizationStore for InMemoryDeviceAuthorizationStore {
    fn insert(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        if records.values().any(|existing| {
            existing.device_code_hash == record.device_code_hash
                || existing.user_code_digest == record.user_code_digest
                || existing.id == record.id
        }) {
            return Err(DeviceAuthorizationStoreError::CodeCollision);
        }
        records.insert(record.id, record);
        Ok(())
    }

    fn by_device_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| &record.device_code_hash == code_hash)
    }

    fn by_user_code_candidates(
        &self,
        candidates: &[VersionedUserCodeDigest],
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| candidates.contains(&record.user_code_digest))
    }

    fn user_code_key_versions(&self) -> Result<Vec<u32>, DeviceAuthorizationStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let versions = records
            .values()
            .map(|record| record.user_code_digest.key_version())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(versions)
    }

    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| match &record.state {
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::VerifyingWebAuthn {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::Issuing {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::Approved {
                approval_id: current,
                ..
            } => current == approval_id,
            _ => false,
        })
    }

    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let current = records
            .get(&replacement.id)
            .ok_or(DeviceAuthorizationStoreError::Conflict)?;
        let Some(next_revision) = expected_revision.checked_add(1) else {
            return Err(DeviceAuthorizationStoreError::Conflict);
        };
        if current.revision != expected_revision
            || replacement.revision != next_revision
            || current.device_code_hash != replacement.device_code_hash
            || current.user_code_digest != replacement.user_code_digest
            || current.scope != replacement.scope
            || current.csr_der != replacement.csr_der
            || current.csr_sha256 != replacement.csr_sha256
            || current.spki_sha256 != replacement.spki_sha256
            || current.created_at_unix_ms != replacement.created_at_unix_ms
            || current.expires_at_unix_ms != replacement.expires_at_unix_ms
        {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        records.insert(replacement.id, replacement);
        Ok(())
    }
}

impl InMemoryDeviceAuthorizationStore {
    fn find(
        &self,
        predicate: impl Fn(&DeviceAuthorizationRecord) -> bool,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        Ok(records.values().find(|record| predicate(record)).cloned())
    }
}

/// Rate-limit port for unauthenticated low-entropy user-code submissions.
///
/// Callers must pass a server-derived, privacy-preserving abuse key, such as a
/// keyed digest of the remote address and pre-auth session. Do not pass a raw
/// IP address or user-supplied value.
pub trait UserCodeAttemptLimiter: Send + Sync {
    fn record_attempt(
        &self,
        abuse_key: &[u8; 32],
        now_unix_ms: u64,
        window_ms: u64,
        maximum_attempts: u32,
    ) -> Result<bool, DeviceAuthorizationRateLimitError>;
}

/// In-process attempt limiter for local testing; use a shared limiter in a
/// multi-instance deployment.
#[derive(Debug, Clone, Default)]
pub struct InMemoryUserCodeAttemptLimiter {
    windows: Arc<Mutex<BTreeMap<[u8; 32], AttemptWindow>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AttemptWindow {
    started_at_unix_ms: u64,
    attempts: u32,
}

impl UserCodeAttemptLimiter for InMemoryUserCodeAttemptLimiter {
    fn record_attempt(
        &self,
        abuse_key: &[u8; 32],
        now_unix_ms: u64,
        window_ms: u64,
        maximum_attempts: u32,
    ) -> Result<bool, DeviceAuthorizationRateLimitError> {
        let mut windows = self
            .windows
            .lock()
            .map_err(|_| DeviceAuthorizationRateLimitError::Unavailable)?;
        windows.retain(|_, window| {
            now_unix_ms < window.started_at_unix_ms
                || now_unix_ms.saturating_sub(window.started_at_unix_ms) < window_ms
        });
        if !windows.contains_key(abuse_key) && windows.len() >= 4_096 {
            return Err(DeviceAuthorizationRateLimitError::Unavailable);
        }
        match windows.get_mut(abuse_key) {
            Some(window) if now_unix_ms < window.started_at_unix_ms => {
                Err(DeviceAuthorizationRateLimitError::ClockMovedBackwards)
            }
            Some(window) if now_unix_ms.saturating_sub(window.started_at_unix_ms) < window_ms => {
                if window.attempts >= maximum_attempts {
                    Ok(false)
                } else {
                    window.attempts = window.attempts.saturating_add(1);
                    Ok(true)
                }
            }
            Some(window) => {
                window.started_at_unix_ms = now_unix_ms;
                window.attempts = 1;
                Ok(true)
            }
            None => {
                windows.insert(
                    *abuse_key,
                    AttemptWindow {
                        started_at_unix_ms: now_unix_ms,
                        attempts: 1,
                    },
                );
                Ok(true)
            }
        }
    }
}

/// Rate-limit failures.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeviceAuthorizationRateLimitError {
    #[error("attempt limiter unavailable")]
    Unavailable,
    #[error("clock moved backwards")]
    ClockMovedBackwards,
}

/// CSR boundary that parses the request, validates its proof of possession,
/// and returns the SHA-256 digest of the SubjectPublicKeyInfo it extracted.
pub trait DeviceCsrValidator: Send + Sync {
    fn validate_and_hash_spki(
        &self,
        csr_der: &[u8],
    ) -> Result<[u8; 32], DeviceAuthorizationPortError>;
}

/// Membership lookup for the approver's trusted upstream identity.
pub trait WorkspaceMembershipPort: Send + Sync {
    fn is_member(
        &self,
        approver: &UserIdentityRef,
        scope: &DeviceAuthorizationScope,
    ) -> Result<bool, DeviceAuthorizationPortError>;
}

/// Server-side WebAuthn authentication boundary.
///
/// `start_authentication` must use credentials registered for the trusted
/// identity in `context` and return standards-compliant request options plus
/// opaque state that can be durably serialized by the caller. `finish_authentication`
/// must bind the assertion to that exact state and context, validate signature,
/// RP ID, origin, user handle, credential status and counter policy, then
/// atomically consume the ceremony and update the credential counter.
///
/// Finishing is idempotent for the same approval ID and assertion digest so a
/// caller can recover after an ambiguous timeout. A `Rejected` result must
/// guarantee that neither the ceremony nor credential counter was consumed;
/// `Unavailable` may have an unknown outcome and must be safe to retry.
pub trait WebAuthnAuthenticationPort: Send + Sync {
    fn start_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
    ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError>;

    fn finish_authentication(
        &self,
        context: &WebAuthnAuthenticationContext,
        opaque_state: &[u8],
        assertion: &[u8],
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationPortError>;
}

/// External CA boundary. Implementations must durably and concurrently
/// deduplicate by enrollment ID, returning the same certificate for every
/// retry of the same request, including after a signer restart. They must
/// honor the exact organization/workspace scope and bind the issued
/// certificate to the supplied SPKI digest. The authorization record is
/// durably reserved as `Issuing` before this port is called, so an error may
/// have an unknown outcome and the manager will retry with the same ID and
/// request parameters.
pub trait DeviceCertificateIssuer: Send + Sync {
    fn issue_device_certificate(
        &self,
        enrollment_id: &DeviceAuthorizationId,
        scope: &DeviceAuthorizationScope,
        csr_der: &[u8],
        expected_spki_sha256: &[u8; 32],
        issued_at_unix_ms: u64,
    ) -> Result<IssuedDeviceCertificate, DeviceAuthorizationPortError>;
}

/// Trusted current time source used after external verifier work.
///
/// Caller-supplied request timestamps are not sufficient for a transition that
/// may cross an authorization TTL while an external port is running.
pub trait DeviceAuthorizationClockPort: Send + Sync {
    fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError>;
}

/// Aggregate implemented by the approval orchestration layer so a completion
/// call can invoke all security checks through one explicit port bundle.
pub trait DeviceApprovalStartPorts: WorkspaceMembershipPort + WebAuthnAuthenticationPort {}

impl<T> DeviceApprovalStartPorts for T where T: WorkspaceMembershipPort + WebAuthnAuthenticationPort {}

/// Aggregate required to complete and issue an approval.
pub trait DeviceApprovalPorts:
    DeviceApprovalStartPorts
    + DeviceCsrValidator
    + DeviceCertificateIssuer
    + DeviceAuthorizationClockPort
{
}

impl<T> DeviceApprovalPorts for T where
    T: DeviceApprovalStartPorts
        + DeviceCsrValidator
        + DeviceCertificateIssuer
        + DeviceAuthorizationClockPort
{
}

/// Non-specific failure from an externally implemented security port.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAuthorizationPortError {
    #[error("external security port rejected the request")]
    Rejected,
    #[error("external security port unavailable")]
    Unavailable,
}

/// Fail-closed device authorization errors. API adapters should avoid exposing
/// code-existence details to unauthenticated callers.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeviceAuthorizationError {
    #[error("invalid device authorization request")]
    InvalidRequest,
    #[error("device authorization policy is invalid")]
    InvalidPolicy,
    #[error("operating-system random source unavailable")]
    RandomUnavailable,
    #[error("trusted clock unavailable")]
    ClockUnavailable,
    #[error("user-code key ring is unavailable or missing a required version")]
    UserCodeKeysUnavailable,
    #[error("device authorization storage unavailable")]
    StorageUnavailable,
    #[error("device authorization changed concurrently")]
    ConcurrentTransition,
    #[error("device authorization revision exhausted")]
    RevisionExhausted,
    #[error("device authorization code collision")]
    CodeCollision,
    #[error("device authorization code is invalid")]
    InvalidCode,
    #[error("device authorization code attempts exceeded")]
    TooManyAttempts,
    #[error("attempt limiter unavailable or clock invalid")]
    AttemptLimiterUnavailable,
    #[error("device authorization expired")]
    Expired,
    #[error("device authorization is pending")]
    Pending,
    #[error("device authorization was denied")]
    Denied,
    #[error("device authorization is already final or in progress")]
    AlreadyFinal,
    #[error("device authorization was already consumed")]
    AlreadyConsumed,
    #[error("device authorization scope mismatch")]
    ScopeMismatch,
    #[error("approver is not a member of the target workspace")]
    MembershipRequired,
    #[error("membership service unavailable")]
    MembershipUnavailable,
    #[error("CSR is invalid")]
    InvalidCsr,
    #[error("CSR validator unavailable")]
    CsrValidatorUnavailable,
    #[error("CSR SubjectPublicKeyInfo binding changed")]
    CsrBindingMismatch,
    #[error("WebAuthn assertion is invalid")]
    InvalidWebAuthnAssertion,
    #[error("no registered WebAuthn credential is available for approval")]
    WebAuthnCredentialRequired,
    #[error("WebAuthn verifier unavailable")]
    WebAuthnUnavailable,
    #[error("certificate signer unavailable or rejected the request")]
    CertificateSigningFailed,
    #[error("certificate issuance outcome is unknown; retry the same approval to recover")]
    CertificateIssuanceInProgress,
    #[error("issued certificate binding does not match the authorization")]
    CertificateBindingMismatch,
    #[error("polling occurred before the required interval")]
    SlowDown { retry_after_ms: u64 },
}

/// Device authorization coordinator. It contains no identity provider or CA
/// implementation; all security decisions that cross a trust boundary go
/// through the explicit ports above.
pub struct DeviceAuthorizationManager<S, L> {
    store: S,
    limiter: L,
    user_code_keys: UserCodeKeyRing,
    policy: DeviceAuthorizationPolicy,
}

impl<S, L> DeviceAuthorizationManager<S, L>
where
    S: DeviceAuthorizationStore,
    L: UserCodeAttemptLimiter,
{
    /// Creates a state machine with validated timing, rate limits, and keys.
    ///
    /// The injected key ring must contain every key version referenced by the
    /// store before the manager can serve user-code lookups.
    pub fn new(
        store: S,
        limiter: L,
        user_code_keys: UserCodeKeyRing,
        policy: DeviceAuthorizationPolicy,
    ) -> Result<Self, DeviceAuthorizationError> {
        if !policy.is_valid() {
            return Err(DeviceAuthorizationError::InvalidPolicy);
        }
        let required_versions = store.user_code_key_versions().map_err(map_store_error)?;
        user_code_keys
            .require_versions(required_versions)
            .map_err(|_| DeviceAuthorizationError::UserCodeKeysUnavailable)?;
        Ok(Self {
            store,
            limiter,
            user_code_keys,
            policy,
        })
    }

    /// Creates a pending enrollment using server-side OS randomness.
    ///
    /// Raw codes are returned only in this response; the store receives
    /// domain-separated SHA-256 digests. The CSR validator must verify both the
    /// CSR encoding and proof of possession before the request is persisted.
    pub fn begin(
        &self,
        request: DeviceAuthorizationRequest,
        now_unix_ms: u64,
        csr_validator: &impl DeviceCsrValidator,
    ) -> Result<DeviceAuthorizationStart, DeviceAuthorizationError> {
        validate_scope(&request.scope)?;
        if request.csr_der.is_empty() || request.csr_der.len() > 16 * 1024 {
            return Err(DeviceAuthorizationError::InvalidRequest);
        }
        let expires_at_unix_ms = now_unix_ms
            .checked_add(self.policy.authorization_ttl_ms)
            .ok_or(DeviceAuthorizationError::InvalidRequest)?;
        let spki_sha256 = match csr_validator.validate_and_hash_spki(&request.csr_der) {
            Ok(hash) => hash,
            Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::InvalidCsr)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::CsrValidatorUnavailable)
            }
        };
        let csr_sha256 = sha256(&request.csr_der);

        // A bounded retry handles the vanishingly unlikely random code collision.
        for _ in 0..3 {
            let enrollment_id = random_array::<16>()?;
            let device_code_bytes = random_array::<32>()?;
            let user_code_entropy = random_array::<8>()?;
            let device_code = encode_hex(&device_code_bytes);
            let user_code = encode_user_code(&user_code_entropy);
            let record = DeviceAuthorizationRecord {
                id: enrollment_id,
                device_code_hash: hash_code(b"device", device_code.as_bytes()),
                user_code_digest: self
                    .user_code_keys
                    .digest_for_storage(&normalize_user_code(&user_code))
                    .map_err(map_user_code_secret_error)?,
                scope: request.scope.clone(),
                csr_der: request.csr_der.clone(),
                csr_sha256,
                spki_sha256,
                created_at_unix_ms: now_unix_ms,
                expires_at_unix_ms,
                poll_interval_ms: self.policy.initial_poll_interval_ms,
                last_poll_at_unix_ms: None,
                revision: 0,
                state: DeviceAuthorizationState::Pending,
            };
            match self.store.insert(record) {
                Ok(()) => {
                    return Ok(DeviceAuthorizationStart {
                        device_code,
                        user_code,
                        expires_at_unix_ms,
                        poll_interval_ms: self.policy.initial_poll_interval_ms,
                    })
                }
                Err(DeviceAuthorizationStoreError::CodeCollision) => continue,
                Err(error) => return Err(map_store_error(error)),
            }
        }
        Err(DeviceAuthorizationError::CodeCollision)
    }

    /// Checks an approval code, exact target scope, and membership, then issues
    /// a one-time WebAuthn challenge for the trusted SSO identity.
    pub fn begin_approval(
        &self,
        user_code: &str,
        scope: &DeviceAuthorizationScope,
        approver: &UserIdentityRef,
        abuse_key: &[u8; 32],
        now_unix_ms: u64,
        ports: &impl DeviceApprovalStartPorts,
    ) -> Result<DeviceApprovalChallenge, DeviceAuthorizationError> {
        self.record_user_code_attempt(abuse_key, now_unix_ms)?;
        validate_scope(scope)?;
        validate_identity(approver)?;
        let user_code = normalize_user_code(user_code);
        if !is_valid_user_code(&user_code) {
            return Err(DeviceAuthorizationError::InvalidCode);
        }
        let mut record = self.lookup_by_user_code(&user_code)?;
        self.expire_if_due(&mut record, now_unix_ms)?;
        if record.scope != *scope {
            return Err(DeviceAuthorizationError::ScopeMismatch);
        }
        match ports.is_member(approver, scope) {
            Ok(true) => {}
            Ok(false) => return Err(DeviceAuthorizationError::MembershipRequired),
            Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::MembershipRequired)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::MembershipUnavailable)
            }
        }

        if let DeviceAuthorizationState::AwaitingWebAuthn {
            approval_id,
            approver: current_approver,
            credential_request_options_json,
            ..
        } = &record.state
        {
            if current_approver == approver {
                return Ok(DeviceApprovalChallenge {
                    approval_id: *approval_id,
                    credential_request_options_json: credential_request_options_json.clone(),
                });
            }
            return Err(DeviceAuthorizationError::AlreadyFinal);
        }
        if !matches!(record.state, DeviceAuthorizationState::Pending) {
            return Err(state_error(&record.state));
        }

        let approval_id = random_array::<16>()?;
        let context = WebAuthnAuthenticationContext {
            approval_id,
            authorization_id: record.id,
            approver: approver.clone(),
            scope: record.scope.clone(),
            csr_sha256: record.csr_sha256,
            spki_sha256: record.spki_sha256,
            expires_at_unix_ms: record.expires_at_unix_ms,
        };
        let start = match ports.start_authentication(&context) {
            Ok(start)
                if !start.credential_request_options_json.is_empty()
                    && start.credential_request_options_json.len() <= 64 * 1024
                    && !start.opaque_state.is_empty()
                    && start.opaque_state.len() <= 64 * 1024 =>
            {
                start
            }
            Ok(_) | Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::WebAuthnCredentialRequired)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::WebAuthnUnavailable)
            }
        };
        let desired_state = DeviceAuthorizationState::AwaitingWebAuthn {
            approval_id,
            approver: approver.clone(),
            credential_request_options_json: start.credential_request_options_json.clone(),
            opaque_state: start.opaque_state,
        };

        // Polling may advance the record revision while verifier start runs.
        // Re-read and retry only while the same enrollment is still pending.
        for _ in 0..5 {
            record.state = desired_state.clone();
            match self.replace(record.clone()) {
                Ok(()) => {
                    return Ok(DeviceApprovalChallenge {
                        approval_id,
                        credential_request_options_json: start.credential_request_options_json,
                    })
                }
                Err(DeviceAuthorizationError::ConcurrentTransition) => {
                    record = self
                        .lookup_by_user_code(&user_code)
                        .map_err(|error| match error {
                            DeviceAuthorizationError::InvalidCode => {
                                DeviceAuthorizationError::ConcurrentTransition
                            }
                            other => other,
                        })?;
                    match &record.state {
                        DeviceAuthorizationState::Pending => {}
                        DeviceAuthorizationState::AwaitingWebAuthn {
                            approval_id: current_id,
                            approver: current_approver,
                            credential_request_options_json,
                            ..
                        } if current_approver == approver => {
                            return Ok(DeviceApprovalChallenge {
                                approval_id: *current_id,
                                credential_request_options_json: credential_request_options_json
                                    .clone(),
                            });
                        }
                        state => return Err(state_error(state)),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    /// Completes an approval only after rechecking membership, CSR/SPKI
    /// binding, and the one-time WebAuthn assertion.
    pub fn complete_approval(
        &self,
        approval_id: &DeviceAuthorizationId,
        assertion: &[u8],
        now_unix_ms: u64,
        ports: &impl DeviceApprovalPorts,
    ) -> Result<(), DeviceAuthorizationError> {
        let mut record = self
            .store
            .by_approval_id(approval_id)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        self.expire_if_due(&mut record, now_unix_ms)?;

        // An Issuing record proves that membership, CSR binding, and the
        // WebAuthn assertion were already checked before the durable CAS.
        // Resume using the persisted request parameters and enrollment ID;
        // re-verifying a one-time assertion here could prevent recovery.
        match &record.state {
            DeviceAuthorizationState::Issuing { .. } => {
                return self.issue_reserved_approval(record, ports)
            }
            DeviceAuthorizationState::Approved { .. } => return Ok(()),
            DeviceAuthorizationState::AwaitingWebAuthn { .. }
            | DeviceAuthorizationState::VerifyingWebAuthn { .. } => {}
            state => return Err(state_error(state)),
        }
        if assertion.is_empty() || assertion.len() > 16 * 1024 {
            return Err(DeviceAuthorizationError::InvalidWebAuthnAssertion);
        }
        let assertion_sha256 = sha256(assertion);
        let (approver, opaque_state) = match &record.state {
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id: current,
                approver,
                opaque_state,
                ..
            } if current == approval_id => (approver.clone(), opaque_state.clone()),
            DeviceAuthorizationState::VerifyingWebAuthn {
                approval_id: current,
                approver,
                assertion_sha256: current_assertion,
                opaque_state,
            } if current == approval_id && current_assertion == &assertion_sha256 => {
                (approver.clone(), opaque_state.clone())
            }
            DeviceAuthorizationState::VerifyingWebAuthn { .. } => {
                return Err(DeviceAuthorizationError::InvalidWebAuthnAssertion)
            }
            _ => return Err(state_error(&record.state)),
        };

        match ports.is_member(&approver, &record.scope) {
            Ok(true) => {}
            Ok(false) | Err(DeviceAuthorizationPortError::Rejected) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::MembershipRequired);
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::MembershipUnavailable);
            }
        }

        let spki_sha256 = match ports.validate_and_hash_spki(&record.csr_der) {
            Ok(hash) => hash,
            Err(DeviceAuthorizationPortError::Rejected) => {
                self.deny_reserved_approval(record, approval_id, &approver, now_unix_ms)?;
                return Err(DeviceAuthorizationError::InvalidCsr);
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::CsrValidatorUnavailable);
            }
        };
        if spki_sha256 != record.spki_sha256 {
            self.deny_reserved_approval(record, approval_id, &approver, now_unix_ms)?;
            return Err(DeviceAuthorizationError::CsrBindingMismatch);
        }

        if sha256(&record.csr_der) != record.csr_sha256 {
            self.deny_reserved_approval(record, approval_id, &approver, now_unix_ms)?;
            return Err(DeviceAuthorizationError::CsrBindingMismatch);
        }

        let context = WebAuthnAuthenticationContext {
            approval_id: *approval_id,
            authorization_id: record.id,
            approver: approver.clone(),
            scope: record.scope.clone(),
            csr_sha256: record.csr_sha256,
            spki_sha256: record.spki_sha256,
            expires_at_unix_ms: record.expires_at_unix_ms,
        };
        record = self.reserve_verification(
            record,
            approval_id,
            &approver,
            &opaque_state,
            &assertion_sha256,
        )?;

        match ports.finish_authentication(&context, &opaque_state, assertion, now_unix_ms) {
            Ok(()) => {}
            Err(DeviceAuthorizationPortError::Rejected) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::InvalidWebAuthnAssertion);
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                // The verifier may have consumed the challenge and advanced
                // the credential counter. Keep this exact assertion resumable.
                return Err(DeviceAuthorizationError::WebAuthnUnavailable);
            }
        }

        // This CAS is the approval linearization point. Denial or expiry may
        // win while WebAuthn runs; in that case the signer is never invoked.
        record = self.reserve_issuance(record, approval_id, &approver, &assertion_sha256, ports)?;
        self.issue_reserved_approval(record, ports)
    }

    fn reserve_verification(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: &DeviceAuthorizationId,
        approver: &UserIdentityRef,
        opaque_state: &[u8],
        assertion_sha256: &[u8; 32],
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::AwaitingWebAuthn {
                    approval_id: current,
                    approver: current_approver,
                    opaque_state: current_state,
                    ..
                } if current == approval_id
                    && current_approver == approver
                    && current_state == opaque_state =>
                {
                    record.state = DeviceAuthorizationState::VerifyingWebAuthn {
                        approval_id: *approval_id,
                        approver: approver.clone(),
                        assertion_sha256: *assertion_sha256,
                        opaque_state: opaque_state.to_vec(),
                    };
                }
                DeviceAuthorizationState::VerifyingWebAuthn {
                    approval_id: current,
                    approver: current_approver,
                    assertion_sha256: current_assertion,
                    opaque_state: current_state,
                } if current == approval_id
                    && current_approver == approver
                    && current_assertion == assertion_sha256
                    && current_state == opaque_state =>
                {
                    return Ok(record)
                }
                state => return Err(state_error(state)),
            }
            match self.replace(record.clone()) {
                Ok(()) => {
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    return Ok(record);
                }
                Err(DeviceAuthorizationError::ConcurrentTransition) => {
                    record = self
                        .store
                        .by_device_code_hash(&record.device_code_hash)
                        .map_err(map_store_error)?
                        .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    fn reserve_issuance(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: &DeviceAuthorizationId,
        approver: &UserIdentityRef,
        assertion_sha256: &[u8; 32],
        clock: &impl DeviceAuthorizationClockPort,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::VerifyingWebAuthn {
                    approval_id: current,
                    approver: current_approver,
                    assertion_sha256: current_assertion,
                    ..
                } if current == approval_id
                    && current_approver == approver
                    && current_assertion == assertion_sha256 =>
                {
                    let issued_at_unix_ms = clock
                        .current_unix_ms()
                        .map_err(|_| DeviceAuthorizationError::ClockUnavailable)?;
                    if issued_at_unix_ms >= record.expires_at_unix_ms {
                        match self.expire_if_due(&mut record, issued_at_unix_ms) {
                            Err(DeviceAuthorizationError::ConcurrentTransition) => {
                                record = self
                                    .store
                                    .by_device_code_hash(&record.device_code_hash)
                                    .map_err(map_store_error)?
                                    .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                                continue;
                            }
                            Err(error) => return Err(error),
                            Ok(()) => return Err(DeviceAuthorizationError::Expired),
                        }
                    }
                    record.state = DeviceAuthorizationState::Issuing {
                        approval_id: *approval_id,
                        approver: approver.clone(),
                        issued_at_unix_ms,
                    };
                }
                DeviceAuthorizationState::Issuing {
                    approval_id: current,
                    ..
                } if current == approval_id => return Ok(record),
                DeviceAuthorizationState::Approved {
                    approval_id: current,
                    ..
                } if current == approval_id => return Ok(record),
                state => return Err(state_error(state)),
            }
            match self.replace(record.clone()) {
                Ok(()) => {
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    return Ok(record);
                }
                Err(DeviceAuthorizationError::ConcurrentTransition) => {
                    record = self
                        .store
                        .by_device_code_hash(&record.device_code_hash)
                        .map_err(map_store_error)?
                        .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    fn issue_reserved_approval(
        &self,
        mut record: DeviceAuthorizationRecord,
        ports: &impl DeviceApprovalPorts,
    ) -> Result<(), DeviceAuthorizationError> {
        let (approval_id, approver, issued_at_unix_ms) = match &record.state {
            DeviceAuthorizationState::Issuing {
                approval_id,
                approver,
                issued_at_unix_ms,
            } => (*approval_id, approver.clone(), *issued_at_unix_ms),
            state => return Err(state_error(state)),
        };
        let certificate = match ports.issue_device_certificate(
            &record.id,
            &record.scope,
            &record.csr_der,
            &record.spki_sha256,
            issued_at_unix_ms,
        ) {
            Ok(certificate) => certificate,
            Err(_) => {
                // A signer error does not prove that the CA did not commit.
                // Keep Issuing and let a retry query/replay the same durable
                // idempotency key. A concurrent retry may already have
                // completed the state transition successfully.
                if self.approval_is_durably_approved(&approval_id)? {
                    return Ok(());
                }
                return Err(DeviceAuthorizationError::CertificateIssuanceInProgress);
            }
        };
        if certificate.certificate_der.is_empty()
            || certificate.serial_number.is_empty()
            || certificate.scope != record.scope
            || certificate.spki_sha256 != record.spki_sha256
            || certificate.not_after_unix_ms <= issued_at_unix_ms
        {
            return Err(DeviceAuthorizationError::CertificateBindingMismatch);
        }

        // Polling and concurrent idempotent retries can advance unrelated CAS
        // revisions while the CA call is running. Re-read and retry the final
        // transition only while the same issuance reservation remains active.
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::Issuing {
                    approval_id: current_id,
                    approver: current_approver,
                    issued_at_unix_ms: current_issued_at,
                } if current_id == &approval_id
                    && current_approver == &approver
                    && *current_issued_at == issued_at_unix_ms => {}
                DeviceAuthorizationState::Approved {
                    approval_id: current_id,
                    certificate: current_certificate,
                    ..
                } if current_id == &approval_id && current_certificate == &certificate => {
                    return Ok(())
                }
                state => return Err(state_error(state)),
            }

            let mut replacement = record.clone();
            replacement.state = DeviceAuthorizationState::Approved {
                approval_id,
                approver: approver.clone(),
                decided_at_unix_ms: issued_at_unix_ms,
                certificate: certificate.clone(),
            };
            match self.replace(replacement) {
                Ok(()) => return Ok(()),
                Err(DeviceAuthorizationError::ConcurrentTransition) => {
                    record = self
                        .store
                        .by_approval_id(&approval_id)
                        .map_err(map_store_error)?
                        .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    fn approval_is_durably_approved(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<bool, DeviceAuthorizationError> {
        let record = self
            .store
            .by_approval_id(approval_id)
            .map_err(map_store_error)?;
        Ok(matches!(
            record.map(|record| record.state),
            Some(DeviceAuthorizationState::Approved {
                approval_id: current,
                ..
            }) if current == *approval_id
        ))
    }

    /// Denies a pending enrollment once. A denial also invalidates any active
    /// WebAuthn challenge for that enrollment.
    pub fn deny(
        &self,
        user_code: &str,
        scope: &DeviceAuthorizationScope,
        approver: &UserIdentityRef,
        abuse_key: &[u8; 32],
        now_unix_ms: u64,
        membership: &impl WorkspaceMembershipPort,
    ) -> Result<(), DeviceAuthorizationError> {
        self.record_user_code_attempt(abuse_key, now_unix_ms)?;
        validate_scope(scope)?;
        validate_identity(approver)?;
        let user_code = normalize_user_code(user_code);
        if !is_valid_user_code(&user_code) {
            return Err(DeviceAuthorizationError::InvalidCode);
        }
        let mut record = self.lookup_by_user_code(&user_code)?;
        self.expire_if_due(&mut record, now_unix_ms)?;
        if record.scope != *scope {
            return Err(DeviceAuthorizationError::ScopeMismatch);
        }
        if !matches!(
            record.state,
            DeviceAuthorizationState::Pending
                | DeviceAuthorizationState::AwaitingWebAuthn { .. }
                | DeviceAuthorizationState::VerifyingWebAuthn { .. }
        ) {
            return Err(state_error(&record.state));
        }
        match membership.is_member(approver, scope) {
            Ok(true) => {}
            Ok(false) | Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::MembershipRequired)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::MembershipUnavailable)
            }
        }
        record.state = DeviceAuthorizationState::Denied {
            approver: approver.clone(),
            decided_at_unix_ms: now_unix_ms,
        };
        self.replace(record)
    }

    /// Polls with the device code and consumes an approved enrollment before
    /// returning its certificate. This provides at-most-once delivery: if the
    /// response is lost after the CAS, the device cannot fetch it again. A
    /// retryable delivery guarantee needs a durable acknowledgment protocol.
    pub fn poll(
        &self,
        device_code: &str,
        now_unix_ms: u64,
    ) -> Result<DeviceAuthorizationPoll, DeviceAuthorizationError> {
        if device_code.len() != 64 || !device_code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DeviceAuthorizationError::InvalidCode);
        }
        let code_hash = hash_code(b"device", device_code.to_ascii_lowercase().as_bytes());
        let mut record = self
            .store
            .by_device_code_hash(&code_hash)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        match record.state {
            DeviceAuthorizationState::Consumed { .. } => {
                return Err(DeviceAuthorizationError::AlreadyConsumed)
            }
            DeviceAuthorizationState::Denied { .. } => {
                return Err(DeviceAuthorizationError::Denied)
            }
            DeviceAuthorizationState::Expired => return Err(DeviceAuthorizationError::Expired),
            _ => {}
        }
        self.expire_if_due(&mut record, now_unix_ms)?;

        if let Some(last_poll_at_unix_ms) = record.last_poll_at_unix_ms {
            let next_allowed_at = last_poll_at_unix_ms.saturating_add(record.poll_interval_ms);
            if now_unix_ms < next_allowed_at {
                record.poll_interval_ms = record
                    .poll_interval_ms
                    .saturating_add(self.policy.slow_down_increment_ms)
                    .min(self.policy.maximum_poll_interval_ms);
                let next_allowed_at = last_poll_at_unix_ms.saturating_add(record.poll_interval_ms);
                let retry_after_ms = next_allowed_at.saturating_sub(now_unix_ms);
                self.replace(record.clone())?;
                return Err(DeviceAuthorizationError::SlowDown { retry_after_ms });
            }
        }

        record.last_poll_at_unix_ms = Some(now_unix_ms);
        match record.state.clone() {
            DeviceAuthorizationState::Pending
            | DeviceAuthorizationState::AwaitingWebAuthn { .. }
            | DeviceAuthorizationState::VerifyingWebAuthn { .. }
            | DeviceAuthorizationState::Issuing { .. } => {
                let interval_ms = record.poll_interval_ms;
                self.replace(record)?;
                Ok(DeviceAuthorizationPoll::Pending { interval_ms })
            }
            DeviceAuthorizationState::Approved {
                approver,
                decided_at_unix_ms,
                certificate,
                ..
            } => {
                record.state = DeviceAuthorizationState::Consumed {
                    approver,
                    decided_at_unix_ms,
                    consumed_at_unix_ms: now_unix_ms,
                };
                self.replace(record)?;
                Ok(DeviceAuthorizationPoll::Approved(certificate))
            }
            DeviceAuthorizationState::Denied { .. } => Err(DeviceAuthorizationError::Denied),
            DeviceAuthorizationState::Consumed { .. } => {
                Err(DeviceAuthorizationError::AlreadyConsumed)
            }
            DeviceAuthorizationState::Expired => Err(DeviceAuthorizationError::Expired),
        }
    }

    fn record_user_code_attempt(
        &self,
        abuse_key: &[u8; 32],
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationError> {
        match self.limiter.record_attempt(
            abuse_key,
            now_unix_ms,
            self.policy.user_code_attempt_window_ms,
            self.policy.maximum_user_code_attempts,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(DeviceAuthorizationError::TooManyAttempts),
            Err(_) => Err(DeviceAuthorizationError::AttemptLimiterUnavailable),
        }
    }

    fn lookup_by_user_code(
        &self,
        normalized_code: &str,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        let candidates = self
            .user_code_keys
            .lookup_candidates(normalized_code)
            .map_err(map_user_code_secret_error)?;
        let record = self
            .store
            .by_user_code_candidates(&candidates)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        match self
            .user_code_keys
            .verify(normalized_code, &record.user_code_digest)
            .map_err(map_user_code_secret_error)?
        {
            true => Ok(record),
            false => Err(DeviceAuthorizationError::InvalidCode),
        }
    }

    fn expire_if_due(
        &self,
        record: &mut DeviceAuthorizationRecord,
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationError> {
        // An in-flight verifier may consume an assertion; issuance may already
        // have committed at the CA. Keep only `Issuing` recoverable after TTL.
        if matches!(record.state, DeviceAuthorizationState::Issuing { .. }) {
            return Ok(());
        }
        if now_unix_ms < record.expires_at_unix_ms {
            return Ok(());
        }
        match record.state {
            DeviceAuthorizationState::Pending
            | DeviceAuthorizationState::AwaitingWebAuthn { .. }
            | DeviceAuthorizationState::VerifyingWebAuthn { .. }
            | DeviceAuthorizationState::Approved { .. } => {
                record.state = DeviceAuthorizationState::Expired;
                self.replace(record.clone())?;
                *record = self
                    .store
                    .by_device_code_hash(&record.device_code_hash)
                    .map_err(map_store_error)?
                    .ok_or(DeviceAuthorizationError::InvalidCode)?;
                Err(DeviceAuthorizationError::Expired)
            }
            DeviceAuthorizationState::Issuing { .. } => Ok(()),
            DeviceAuthorizationState::Expired => Err(DeviceAuthorizationError::Expired),
            DeviceAuthorizationState::Denied { .. } => Err(DeviceAuthorizationError::Denied),
            DeviceAuthorizationState::Consumed { .. } => {
                Err(DeviceAuthorizationError::AlreadyConsumed)
            }
        }
    }

    fn release_approval(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<(), DeviceAuthorizationError> {
        if !matches!(
            record.state,
            DeviceAuthorizationState::AwaitingWebAuthn { approval_id: current, .. }
                | DeviceAuthorizationState::VerifyingWebAuthn { approval_id: current, .. }
                if &current == approval_id
        ) {
            return Err(DeviceAuthorizationError::AlreadyFinal);
        }
        record.state = DeviceAuthorizationState::Pending;
        self.replace(record)
    }

    fn deny_reserved_approval(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: &DeviceAuthorizationId,
        approver: &UserIdentityRef,
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationError> {
        if !matches!(
            record.state,
            DeviceAuthorizationState::AwaitingWebAuthn { approval_id: current, .. }
                | DeviceAuthorizationState::VerifyingWebAuthn { approval_id: current, .. }
                if &current == approval_id
        ) {
            return Err(DeviceAuthorizationError::AlreadyFinal);
        }
        record.state = DeviceAuthorizationState::Denied {
            approver: approver.clone(),
            decided_at_unix_ms: now_unix_ms,
        };
        self.replace(record)
    }

    fn replace(
        &self,
        mut replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationError> {
        let expected_revision = replacement.revision;
        replacement.revision = expected_revision
            .checked_add(1)
            .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
        self.store
            .compare_and_swap(expected_revision, replacement)
            .map_err(map_store_error)
    }
}

fn validate_scope(scope: &DeviceAuthorizationScope) -> Result<(), DeviceAuthorizationError> {
    if scope.organization_id.trim().is_empty()
        || scope.workspace_id.trim().is_empty()
        || scope.organization_id.len() > 256
        || scope.workspace_id.len() > 256
        || scope.organization_id != scope.organization_id.trim()
        || scope.workspace_id != scope.workspace_id.trim()
    {
        return Err(DeviceAuthorizationError::InvalidRequest);
    }
    Ok(())
}

fn validate_identity(identity: &UserIdentityRef) -> Result<(), DeviceAuthorizationError> {
    if identity.issuer.trim().is_empty()
        || identity.subject.trim().is_empty()
        || identity.issuer.len() > 2_048
        || identity.subject.len() > 2_048
    {
        return Err(DeviceAuthorizationError::InvalidRequest);
    }
    Ok(())
}

fn normalize_user_code(value: &str) -> String {
    value
        .bytes()
        .filter(|byte| *byte != b'-' && !byte.is_ascii_whitespace())
        .map(|byte| byte.to_ascii_uppercase() as char)
        .collect()
}

fn is_valid_user_code(value: &str) -> bool {
    value.len() == 12
        && value
            .bytes()
            .all(|byte| b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(&byte))
}

fn encode_user_code(entropy: &[u8; 8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let value = u64::from_be_bytes(*entropy);
    let mut code = [b'A'; 12];
    for (index, byte) in code.iter_mut().enumerate() {
        let shift = 59 - index * 5;
        *byte = ALPHABET[((value >> shift) & 0x1f) as usize];
    }
    let mut raw = String::with_capacity(code.len());
    for byte in code {
        raw.push(char::from(byte));
    }
    format!("{}-{}-{}", &raw[0..4], &raw[4..8], &raw[8..12])
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hash_code(kind: &[u8], code: &[u8]) -> DeviceAuthorizationCodeHash {
    // This digest is used only for 256-bit random device bearer codes.
    // Human-entered user codes use the injected, versioned HMAC key ring.
    let mut hasher = Sha256::new();
    hasher.update(b"cyrene-device-authorization-v1\0");
    hasher.update(kind);
    hasher.update(b"\0");
    hasher.update(code);
    hasher.finalize().into()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn random_array<const N: usize>() -> Result<[u8; N], DeviceAuthorizationError> {
    let mut output = [0; N];
    getrandom::getrandom(&mut output).map_err(|_| DeviceAuthorizationError::RandomUnavailable)?;
    Ok(output)
}

fn state_error(state: &DeviceAuthorizationState) -> DeviceAuthorizationError {
    match state {
        DeviceAuthorizationState::Pending => DeviceAuthorizationError::Pending,
        DeviceAuthorizationState::AwaitingWebAuthn { .. }
        | DeviceAuthorizationState::VerifyingWebAuthn { .. }
        | DeviceAuthorizationState::Issuing { .. }
        | DeviceAuthorizationState::Approved { .. } => DeviceAuthorizationError::AlreadyFinal,
        DeviceAuthorizationState::Denied { .. } => DeviceAuthorizationError::Denied,
        DeviceAuthorizationState::Consumed { .. } => DeviceAuthorizationError::AlreadyConsumed,
        DeviceAuthorizationState::Expired => DeviceAuthorizationError::Expired,
    }
}

fn map_store_error(error: DeviceAuthorizationStoreError) -> DeviceAuthorizationError {
    match error {
        DeviceAuthorizationStoreError::CodeCollision => DeviceAuthorizationError::CodeCollision,
        DeviceAuthorizationStoreError::Conflict => DeviceAuthorizationError::ConcurrentTransition,
        DeviceAuthorizationStoreError::Unavailable => DeviceAuthorizationError::StorageUnavailable,
    }
}

fn map_user_code_secret_error(error: UserCodeSecretError) -> DeviceAuthorizationError {
    match error {
        UserCodeSecretError::InvalidUserCode => DeviceAuthorizationError::InvalidCode,
        _ => DeviceAuthorizationError::UserCodeKeysUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Condvar};
    use std::thread;

    struct TestCsrValidator;

    impl DeviceCsrValidator for TestCsrValidator {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    struct TestApprovalPorts;

    impl DeviceCsrValidator for TestApprovalPorts {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    impl WorkspaceMembershipPort for TestApprovalPorts {
        fn is_member(
            &self,
            _approver: &UserIdentityRef,
            _scope: &DeviceAuthorizationScope,
        ) -> Result<bool, DeviceAuthorizationPortError> {
            Ok(true)
        }
    }

    impl WebAuthnAuthenticationPort for TestApprovalPorts {
        fn start_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
        ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
            test_webauthn_start(context)
        }

        fn finish_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
            opaque_state: &[u8],
            assertion: &[u8],
            _now_unix_ms: u64,
        ) -> Result<(), DeviceAuthorizationPortError> {
            if test_opaque_state_matches(context, opaque_state)
                && assertion == b"test-only-valid-assertion"
            {
                Ok(())
            } else {
                Err(DeviceAuthorizationPortError::Rejected)
            }
        }
    }

    impl DeviceCertificateIssuer for TestApprovalPorts {
        fn issue_device_certificate(
            &self,
            _enrollment_id: &DeviceAuthorizationId,
            scope: &DeviceAuthorizationScope,
            _csr_der: &[u8],
            expected_spki_sha256: &[u8; 32],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceAuthorizationPortError> {
            Ok(IssuedDeviceCertificate {
                certificate_der: b"test-only-not-a-real-certificate".to_vec(),
                serial_number: vec![1],
                scope: scope.clone(),
                spki_sha256: *expected_spki_sha256,
                not_after_unix_ms: issued_at_unix_ms.saturating_add(60_000),
            })
        }
    }

    impl DeviceAuthorizationClockPort for TestApprovalPorts {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(300)
        }
    }

    fn test_webauthn_start(
        context: &WebAuthnAuthenticationContext,
    ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
        Ok(WebAuthnAuthenticationStart {
            credential_request_options_json: b"{\"challenge\":\"test-bound-challenge\"}".to_vec(),
            opaque_state: test_opaque_state(context),
        })
    }

    fn test_opaque_state(context: &WebAuthnAuthenticationContext) -> Vec<u8> {
        let mut state = Vec::new();
        state.extend_from_slice(&context.approval_id);
        state.extend_from_slice(&context.authorization_id);
        for value in [
            context.scope.organization_id.as_bytes(),
            context.scope.workspace_id.as_bytes(),
        ] {
            state.extend_from_slice(&(value.len() as u64).to_be_bytes());
            state.extend_from_slice(value);
        }
        state.extend_from_slice(&context.csr_sha256);
        state.extend_from_slice(&context.spki_sha256);
        state.extend_from_slice(&context.expires_at_unix_ms.to_be_bytes());
        state
    }

    fn test_opaque_state_matches(
        context: &WebAuthnAuthenticationContext,
        opaque_state: &[u8],
    ) -> bool {
        opaque_state == test_opaque_state(context).as_slice()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum VerifierBehavior {
        Block,
        UnavailableOnce,
    }

    #[derive(Default)]
    struct CoordinatedApprovalState {
        verifier_released: bool,
        verifier_calls: usize,
        credential_counter_updates: usize,
        consumed_assertions: BTreeMap<DeviceAuthorizationId, [u8; 32]>,
        issuer_calls: usize,
    }

    struct CoordinatedApprovalPorts {
        behavior: VerifierBehavior,
        state: Mutex<CoordinatedApprovalState>,
        changed: Condvar,
        verifier_started: mpsc::Sender<()>,
        current_time_unix_ms: AtomicU64,
    }

    impl CoordinatedApprovalPorts {
        fn new(
            behavior: VerifierBehavior,
            current_time_unix_ms: u64,
        ) -> (Self, mpsc::Receiver<()>) {
            let (verifier_started, started_rx) = mpsc::channel();
            (
                Self {
                    behavior,
                    state: Mutex::new(CoordinatedApprovalState::default()),
                    changed: Condvar::new(),
                    verifier_started,
                    current_time_unix_ms: AtomicU64::new(current_time_unix_ms),
                },
                started_rx,
            )
        }

        fn release_verifier(&self) {
            let mut state = self.state.lock().expect("verifier state");
            state.verifier_released = true;
            self.changed.notify_all();
        }

        fn set_current_time(&self, now_unix_ms: u64) {
            self.current_time_unix_ms
                .store(now_unix_ms, Ordering::SeqCst);
        }

        fn credential_counter_updates(&self) -> usize {
            self.state
                .lock()
                .expect("verifier state")
                .credential_counter_updates
        }

        fn verifier_calls(&self) -> usize {
            self.state.lock().expect("verifier state").verifier_calls
        }

        fn issuer_calls(&self) -> usize {
            self.state.lock().expect("verifier state").issuer_calls
        }
    }

    impl DeviceCsrValidator for CoordinatedApprovalPorts {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    impl WorkspaceMembershipPort for CoordinatedApprovalPorts {
        fn is_member(
            &self,
            _approver: &UserIdentityRef,
            _scope: &DeviceAuthorizationScope,
        ) -> Result<bool, DeviceAuthorizationPortError> {
            Ok(true)
        }
    }

    impl WebAuthnAuthenticationPort for CoordinatedApprovalPorts {
        fn start_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
        ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
            test_webauthn_start(context)
        }

        fn finish_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
            opaque_state: &[u8],
            assertion: &[u8],
            _now_unix_ms: u64,
        ) -> Result<(), DeviceAuthorizationPortError> {
            if !test_opaque_state_matches(context, opaque_state)
                || assertion != b"test-only-valid-assertion"
            {
                return Err(DeviceAuthorizationPortError::Rejected);
            }

            let mut state = self.state.lock().expect("verifier state");
            state.verifier_calls += 1;
            let _ = self.verifier_started.send(());
            if self.behavior == VerifierBehavior::Block {
                while !state.verifier_released {
                    state = self.changed.wait(state).expect("verifier state");
                }
            }

            let assertion_sha256 = sha256(assertion);
            match state.consumed_assertions.get(&context.approval_id) {
                Some(consumed) if consumed == &assertion_sha256 => Ok(()),
                Some(_) => Err(DeviceAuthorizationPortError::Rejected),
                None => {
                    state
                        .consumed_assertions
                        .insert(context.approval_id, assertion_sha256);
                    state.credential_counter_updates += 1;
                    if self.behavior == VerifierBehavior::UnavailableOnce {
                        Err(DeviceAuthorizationPortError::Unavailable)
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }

    impl DeviceCertificateIssuer for CoordinatedApprovalPorts {
        fn issue_device_certificate(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            scope: &DeviceAuthorizationScope,
            _csr_der: &[u8],
            expected_spki_sha256: &[u8; 32],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceAuthorizationPortError> {
            self.state.lock().expect("verifier state").issuer_calls += 1;
            Ok(IssuedDeviceCertificate {
                certificate_der: b"test-only-coordinated-certificate".to_vec(),
                serial_number: vec![3],
                scope: scope.clone(),
                spki_sha256: *expected_spki_sha256,
                not_after_unix_ms: issued_at_unix_ms.saturating_add(60_000),
            })
        }
    }

    impl DeviceAuthorizationClockPort for CoordinatedApprovalPorts {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(self.current_time_unix_ms.load(Ordering::SeqCst))
        }
    }

    #[derive(Default)]
    struct BlockingIssuerState {
        in_flight: bool,
        released: bool,
        duplicate_waiters: usize,
        side_effect_count: usize,
        certificate: Option<IssuedDeviceCertificate>,
    }

    struct BlockingApprovalPorts {
        state: Mutex<BlockingIssuerState>,
        changed: Condvar,
        entered: mpsc::Sender<()>,
    }

    impl BlockingApprovalPorts {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (entered, entered_rx) = mpsc::channel();
            (
                Self {
                    state: Mutex::new(BlockingIssuerState::default()),
                    changed: Condvar::new(),
                    entered,
                },
                entered_rx,
            )
        }

        fn release_signer(&self) {
            let mut state = self.state.lock().expect("signer state");
            state.released = true;
            self.changed.notify_all();
        }

        fn wait_for_duplicate_retry(&self) {
            let mut state = self.state.lock().expect("signer state");
            while state.duplicate_waiters == 0 {
                state = self.changed.wait(state).expect("signer state");
            }
        }

        fn side_effect_count(&self) -> usize {
            self.state.lock().expect("signer state").side_effect_count
        }
    }

    impl DeviceCsrValidator for BlockingApprovalPorts {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    impl WorkspaceMembershipPort for BlockingApprovalPorts {
        fn is_member(
            &self,
            _approver: &UserIdentityRef,
            _scope: &DeviceAuthorizationScope,
        ) -> Result<bool, DeviceAuthorizationPortError> {
            Ok(true)
        }
    }

    impl WebAuthnAuthenticationPort for BlockingApprovalPorts {
        fn start_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
        ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
            test_webauthn_start(context)
        }

        fn finish_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
            opaque_state: &[u8],
            assertion: &[u8],
            _now_unix_ms: u64,
        ) -> Result<(), DeviceAuthorizationPortError> {
            if test_opaque_state_matches(context, opaque_state)
                && assertion == b"test-only-valid-assertion"
            {
                Ok(())
            } else {
                Err(DeviceAuthorizationPortError::Rejected)
            }
        }
    }

    impl DeviceCertificateIssuer for BlockingApprovalPorts {
        fn issue_device_certificate(
            &self,
            _enrollment_id: &DeviceAuthorizationId,
            scope: &DeviceAuthorizationScope,
            _csr_der: &[u8],
            expected_spki_sha256: &[u8; 32],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceAuthorizationPortError> {
            let mut state = self.state.lock().expect("signer state");
            if let Some(certificate) = &state.certificate {
                return Ok(certificate.clone());
            }
            if state.in_flight {
                state.duplicate_waiters += 1;
                self.changed.notify_all();
                while state.certificate.is_none() {
                    state = self.changed.wait(state).expect("signer state");
                }
                return Ok(state.certificate.clone().expect("issued certificate"));
            }

            state.in_flight = true;
            state.side_effect_count += 1;
            let _ = self.entered.send(());
            while !state.released {
                state = self.changed.wait(state).expect("signer state");
            }
            let certificate = IssuedDeviceCertificate {
                certificate_der: b"test-only-idempotent-certificate".to_vec(),
                serial_number: vec![1],
                scope: scope.clone(),
                spki_sha256: *expected_spki_sha256,
                not_after_unix_ms: issued_at_unix_ms.saturating_add(60_000),
            };
            state.certificate = Some(certificate.clone());
            state.in_flight = false;
            self.changed.notify_all();
            Ok(certificate)
        }
    }

    impl DeviceAuthorizationClockPort for BlockingApprovalPorts {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(300)
        }
    }

    #[derive(Default)]
    struct UncertainOnceApprovalPorts {
        state: Mutex<Option<(DeviceAuthorizationId, IssuedDeviceCertificate)>>,
        calls: Mutex<usize>,
    }

    impl DeviceCsrValidator for UncertainOnceApprovalPorts {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    impl WorkspaceMembershipPort for UncertainOnceApprovalPorts {
        fn is_member(
            &self,
            _approver: &UserIdentityRef,
            _scope: &DeviceAuthorizationScope,
        ) -> Result<bool, DeviceAuthorizationPortError> {
            Ok(true)
        }
    }

    impl WebAuthnAuthenticationPort for UncertainOnceApprovalPorts {
        fn start_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
        ) -> Result<WebAuthnAuthenticationStart, DeviceAuthorizationPortError> {
            test_webauthn_start(context)
        }

        fn finish_authentication(
            &self,
            context: &WebAuthnAuthenticationContext,
            opaque_state: &[u8],
            assertion: &[u8],
            _now_unix_ms: u64,
        ) -> Result<(), DeviceAuthorizationPortError> {
            if test_opaque_state_matches(context, opaque_state)
                && assertion == b"test-only-valid-assertion"
            {
                Ok(())
            } else {
                Err(DeviceAuthorizationPortError::Rejected)
            }
        }
    }

    impl DeviceCertificateIssuer for UncertainOnceApprovalPorts {
        fn issue_device_certificate(
            &self,
            enrollment_id: &DeviceAuthorizationId,
            scope: &DeviceAuthorizationScope,
            _csr_der: &[u8],
            expected_spki_sha256: &[u8; 32],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceAuthorizationPortError> {
            let mut calls = self.calls.lock().expect("signer call count");
            *calls += 1;
            let mut state = self.state.lock().expect("signer state");
            if let Some((existing_id, certificate)) = &*state {
                return if existing_id == enrollment_id {
                    Ok(certificate.clone())
                } else {
                    Err(DeviceAuthorizationPortError::Rejected)
                };
            }
            let certificate = IssuedDeviceCertificate {
                certificate_der: b"test-only-committed-before-timeout".to_vec(),
                serial_number: vec![2],
                scope: scope.clone(),
                spki_sha256: *expected_spki_sha256,
                not_after_unix_ms: issued_at_unix_ms.saturating_add(60_000),
            };
            *state = Some((*enrollment_id, certificate));
            // Model a lost response after the CA committed the certificate.
            Err(DeviceAuthorizationPortError::Unavailable)
        }
    }

    impl DeviceAuthorizationClockPort for UncertainOnceApprovalPorts {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(300)
        }
    }

    fn manager(
    ) -> DeviceAuthorizationManager<InMemoryDeviceAuthorizationStore, InMemoryUserCodeAttemptLimiter>
    {
        manager_with_store(InMemoryDeviceAuthorizationStore::default())
    }

    fn manager_with_store(
        store: InMemoryDeviceAuthorizationStore,
    ) -> DeviceAuthorizationManager<InMemoryDeviceAuthorizationStore, InMemoryUserCodeAttemptLimiter>
    {
        DeviceAuthorizationManager::new(
            store,
            InMemoryUserCodeAttemptLimiter::default(),
            test_user_code_key_ring(),
            test_policy(),
        )
        .expect("valid test policy")
    }

    fn test_policy() -> DeviceAuthorizationPolicy {
        DeviceAuthorizationPolicy {
            authorization_ttl_ms: 60_000,
            initial_poll_interval_ms: 1_000,
            maximum_poll_interval_ms: 5_000,
            slow_down_increment_ms: 1_000,
            user_code_attempt_window_ms: 60_000,
            maximum_user_code_attempts: 3,
        }
    }

    fn test_user_code_key_ring() -> UserCodeKeyRing {
        UserCodeKeyRing::new(1, BTreeMap::from([(1, [0x5a; 32])]))
            .expect("valid test user-code key ring")
    }

    fn scope() -> DeviceAuthorizationScope {
        DeviceAuthorizationScope {
            organization_id: "org-1".to_string(),
            workspace_id: "workspace-1".to_string(),
        }
    }

    fn user() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "approver-1".to_string(),
        }
    }

    fn start<S, L>(manager: &DeviceAuthorizationManager<S, L>) -> DeviceAuthorizationStart
    where
        S: DeviceAuthorizationStore,
        L: UserCodeAttemptLimiter,
    {
        manager
            .begin(
                DeviceAuthorizationRequest {
                    scope: scope(),
                    csr_der: b"test-only-csr".to_vec(),
                },
                100,
                &TestCsrValidator,
            )
            .expect("begin enrollment")
    }

    #[test]
    fn webauthn_start_persists_state_bound_to_authorization_scope_and_csr() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[3; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");

        let user_code_digest = manager
            .user_code_keys
            .digest_for_storage(&normalize_user_code(&start.user_code))
            .expect("digest user code");
        let record = store
            .by_user_code_candidates(&[user_code_digest])
            .expect("read record")
            .expect("authorization record");
        let (approval_id, options, opaque_state) = match &record.state {
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id,
                credential_request_options_json,
                opaque_state,
                ..
            } => (*approval_id, credential_request_options_json, opaque_state),
            state => panic!("unexpected state: {state:?}"),
        };
        assert_eq!(approval_id, challenge.approval_id);
        assert_eq!(options, b"{\"challenge\":\"test-bound-challenge\"}");
        assert_eq!(&challenge.credential_request_options_json, options);

        let context = WebAuthnAuthenticationContext {
            approval_id,
            authorization_id: record.id,
            approver: user(),
            scope: record.scope.clone(),
            csr_sha256: record.csr_sha256,
            spki_sha256: record.spki_sha256,
            expires_at_unix_ms: record.expires_at_unix_ms,
        };
        assert_eq!(opaque_state.as_slice(), test_opaque_state(&context));

        let retry = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[4; 32],
                201,
                &TestApprovalPorts,
            )
            .expect("retry returns the same persisted ceremony");
        assert_eq!(retry, challenge);
    }

    #[test]
    fn hmac_key_rotation_reads_old_user_codes_and_fails_on_missing_versions() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let old_ring =
            UserCodeKeyRing::new(1, BTreeMap::from([(1, [0x11; 32])])).expect("old key ring");
        let old_manager = DeviceAuthorizationManager::new(
            store.clone(),
            InMemoryUserCodeAttemptLimiter::default(),
            old_ring,
            test_policy(),
        )
        .expect("manager with old version");
        let old_authorization = start(&old_manager);

        let rotated_ring =
            UserCodeKeyRing::new(2, BTreeMap::from([(1, [0x11; 32]), (2, [0x22; 32])]))
                .expect("rotated key ring retains old key");
        let rotated_manager = DeviceAuthorizationManager::new(
            store.clone(),
            InMemoryUserCodeAttemptLimiter::default(),
            rotated_ring,
            test_policy(),
        )
        .expect("manager accepts all stored versions");
        rotated_manager
            .begin_approval(
                &old_authorization.user_code,
                &scope(),
                &user(),
                &[16; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("old user code remains readable during rotation");

        let new_authorization = start(&rotated_manager);
        let record = store
            .by_device_code_hash(&hash_code(
                b"device",
                new_authorization
                    .device_code
                    .to_ascii_lowercase()
                    .as_bytes(),
            ))
            .expect("read new record")
            .expect("authorization");
        assert_eq!(record.user_code_digest.key_version(), 2);

        let missing_old_key =
            UserCodeKeyRing::new(2, BTreeMap::from([(2, [0x22; 32])])).expect("new-only key ring");
        assert!(matches!(
            DeviceAuthorizationManager::new(
                store,
                InMemoryUserCodeAttemptLimiter::default(),
                missing_old_key,
                test_policy(),
            ),
            Err(DeviceAuthorizationError::UserCodeKeysUnavailable)
        ));
    }

    #[test]
    fn approval_is_scope_bound_and_certificate_delivery_is_one_time() {
        let manager = manager();
        let start = start(&manager);
        assert_eq!(start.user_code.len(), 14);
        assert_eq!(start.device_code.len(), 64);

        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[4; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        manager
            .complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &TestApprovalPorts,
            )
            .expect("approval");

        let certificate = match manager.poll(&start.device_code, 1_100).expect("first poll") {
            DeviceAuthorizationPoll::Approved(certificate) => certificate,
            result => panic!("unexpected poll result: {result:?}"),
        };
        assert_eq!(certificate.scope, scope());
        assert_eq!(
            manager.poll(&start.device_code, 2_200),
            Err(DeviceAuthorizationError::AlreadyConsumed)
        );
    }

    #[test]
    fn denial_cannot_win_after_the_issuer_reservation_is_durable() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = Arc::new(manager_with_store(store.clone()));
        let start = start(manager.as_ref());
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[4; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let (ports, entered_rx) = BlockingApprovalPorts::new();
        let ports = Arc::new(ports);

        let approval_manager = Arc::clone(&manager);
        let approval_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let approval = thread::spawn(move || {
            approval_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                approval_ports.as_ref(),
            )
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("signer was entered after Issuing was committed");

        let user_code_digest = manager
            .user_code_keys
            .digest_for_storage(&normalize_user_code(&start.user_code))
            .expect("digest user code");
        let reserved = store
            .by_user_code_candidates(&[user_code_digest])
            .expect("read record")
            .expect("authorization");
        assert!(matches!(
            reserved.state,
            DeviceAuthorizationState::Issuing { .. }
        ));
        assert_eq!(reserved.revision, 3);
        assert_eq!(
            manager.deny(
                &start.user_code,
                &scope(),
                &user(),
                &[5; 32],
                301,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::AlreadyFinal)
        );
        assert!(matches!(
            store
                .by_user_code_candidates(&[user_code_digest])
                .expect("read after denial")
                .expect("authorization")
                .state,
            DeviceAuthorizationState::Issuing { .. }
        ));

        ports.release_signer();
        assert_eq!(approval.join().expect("approval thread"), Ok(()));
        let approved = store
            .by_approval_id(&challenge.approval_id)
            .expect("read approved record")
            .expect("approval remains addressable");
        assert!(matches!(
            approved.state,
            DeviceAuthorizationState::Approved { .. }
        ));
        assert_eq!(ports.side_effect_count(), 1);
    }

    #[test]
    fn concurrent_and_repeated_approval_attempts_share_one_issuer_result() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = Arc::new(manager_with_store(store));
        let start = start(manager.as_ref());
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[4; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let (ports, entered_rx) = BlockingApprovalPorts::new();
        let ports = Arc::new(ports);

        let first_manager = Arc::clone(&manager);
        let first_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let first = thread::spawn(move || {
            first_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                first_ports.as_ref(),
            )
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("first signer call entered");

        let retry_manager = Arc::clone(&manager);
        let retry_ports = Arc::clone(&ports);
        let retry_id = challenge.approval_id;
        let retry = thread::spawn(move || {
            retry_manager.complete_approval(&retry_id, &[], 301, retry_ports.as_ref())
        });
        ports.wait_for_duplicate_retry();
        assert_eq!(ports.side_effect_count(), 1);

        ports.release_signer();
        assert_eq!(first.join().expect("first approval"), Ok(()));
        assert_eq!(retry.join().expect("repeated approval"), Ok(()));
        assert_eq!(
            manager.complete_approval(&challenge.approval_id, &[], 302, ports.as_ref()),
            Ok(())
        );
        assert_eq!(ports.side_effect_count(), 1);
    }

    #[test]
    fn an_uncertain_ca_response_recovers_from_the_persisted_issuing_state() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[4; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let ports = UncertainOnceApprovalPorts::default();

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::CertificateIssuanceInProgress)
        );
        let issuing = store
            .by_approval_id(&challenge.approval_id)
            .expect("read reservation")
            .expect("issuance remains recoverable");
        assert!(matches!(
            issuing.state,
            DeviceAuthorizationState::Issuing { .. }
        ));

        // A new manager models process recovery over the same durable store.
        let restarted_manager = manager_with_store(store.clone());
        assert_eq!(
            restarted_manager.complete_approval(&challenge.approval_id, &[], 400, &ports),
            Ok(())
        );
        let approved = store
            .by_approval_id(&challenge.approval_id)
            .expect("read recovered state")
            .expect("approved record");
        assert!(matches!(
            approved.state,
            DeviceAuthorizationState::Approved { .. }
        ));
        assert_eq!(*ports.calls.lock().expect("signer call count"), 2);
    }

    #[test]
    fn scope_mismatch_expiration_and_user_code_guesses_fail_closed() {
        let manager = manager();
        let start = start(&manager);
        let wrong_scope = DeviceAuthorizationScope {
            organization_id: "org-1".to_string(),
            workspace_id: "workspace-2".to_string(),
        };
        assert_eq!(
            manager.begin_approval(
                &start.user_code,
                &wrong_scope,
                &user(),
                &[8; 32],
                200,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::ScopeMismatch)
        );

        assert_eq!(
            manager.begin_approval(
                "AAAA-AAAA-AAAA",
                &scope(),
                &user(),
                &[9; 32],
                201,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::InvalidCode)
        );
        assert_eq!(
            manager.begin_approval(
                "AAAA-AAAA-AAAA",
                &scope(),
                &user(),
                &[9; 32],
                202,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::InvalidCode)
        );
        assert_eq!(
            manager.begin_approval(
                "AAAA-AAAA-AAAA",
                &scope(),
                &user(),
                &[9; 32],
                203,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::InvalidCode)
        );
        assert_eq!(
            manager.begin_approval(
                "AAAA-AAAA-AAAA",
                &scope(),
                &user(),
                &[9; 32],
                204,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::TooManyAttempts)
        );

        assert_eq!(
            manager.poll(&start.device_code, 60_100),
            Err(DeviceAuthorizationError::Expired)
        );
    }

    #[test]
    fn early_poll_slowdown_increases_the_required_interval() {
        let manager = manager();
        let start = start(&manager);
        assert_eq!(
            manager.poll(&start.device_code, 200),
            Ok(DeviceAuthorizationPoll::Pending { interval_ms: 1_000 })
        );
        assert_eq!(
            manager.poll(&start.device_code, 201),
            Err(DeviceAuthorizationError::SlowDown {
                retry_after_ms: 1_999,
            })
        );
        assert_eq!(
            manager.poll(&start.device_code, 2_200),
            Ok(DeviceAuthorizationPoll::Pending { interval_ms: 2_000 })
        );
    }

    #[test]
    fn denial_is_terminal_and_cannot_be_replayed_as_approval() {
        let manager = manager();
        let start = start(&manager);
        manager
            .deny(
                &start.user_code,
                &scope(),
                &user(),
                &[6; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("first denial");

        assert_eq!(
            manager.deny(
                &start.user_code,
                &scope(),
                &user(),
                &[6; 32],
                201,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::Denied)
        );
        assert_eq!(
            manager.begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[7; 32],
                202,
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::Denied)
        );
        assert_eq!(
            manager.poll(&start.device_code, 203),
            Err(DeviceAuthorizationError::Denied)
        );
    }

    #[test]
    fn poll_revision_change_during_verifier_is_reloaded_before_issuing() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = Arc::new(manager_with_store(store.clone()));
        let start = start(manager.as_ref());
        let (ports, verifier_started) = CoordinatedApprovalPorts::new(VerifierBehavior::Block, 300);
        let ports = Arc::new(ports);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[10; 32],
                200,
                ports.as_ref(),
            )
            .expect("challenge");

        let approval_manager = Arc::clone(&manager);
        let approval_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let approval = thread::spawn(move || {
            approval_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                approval_ports.as_ref(),
            )
        });
        verifier_started
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("verifier call entered");

        ports.set_current_time(1_200);
        assert_eq!(
            manager.poll(&start.device_code, 1_200),
            Ok(DeviceAuthorizationPoll::Pending { interval_ms: 1_000 })
        );
        let verifying = store
            .by_device_code_hash(&hash_code(
                b"device",
                start.device_code.to_ascii_lowercase().as_bytes(),
            ))
            .expect("read after poll")
            .expect("authorization");
        assert!(matches!(
            verifying.state,
            DeviceAuthorizationState::VerifyingWebAuthn { .. }
        ));

        ports.release_verifier();
        assert_eq!(approval.join().expect("approval thread"), Ok(()));
        assert_eq!(ports.issuer_calls(), 1);
        assert!(matches!(
            store
                .by_approval_id(&challenge.approval_id)
                .expect("read approval")
                .expect("approval record")
                .state,
            DeviceAuthorizationState::Approved { .. }
        ));
    }

    #[test]
    fn denial_can_win_while_verifier_runs_and_prevents_signer_call() {
        let manager = Arc::new(manager());
        let start = start(manager.as_ref());
        let (ports, verifier_started) = CoordinatedApprovalPorts::new(VerifierBehavior::Block, 300);
        let ports = Arc::new(ports);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[11; 32],
                200,
                ports.as_ref(),
            )
            .expect("challenge");

        let approval_manager = Arc::clone(&manager);
        let approval_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let approval = thread::spawn(move || {
            approval_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                approval_ports.as_ref(),
            )
        });
        verifier_started
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("verifier call entered");

        manager
            .deny(
                &start.user_code,
                &scope(),
                &user(),
                &[12; 32],
                301,
                ports.as_ref(),
            )
            .expect("deny wins before Issuing");
        ports.release_verifier();
        assert_eq!(
            approval.join().expect("approval thread"),
            Err(DeviceAuthorizationError::Denied)
        );
        assert_eq!(ports.issuer_calls(), 0);
    }

    #[test]
    fn expiry_can_win_while_verifier_runs_and_prevents_signer_call() {
        let manager = Arc::new(manager());
        let start = start(manager.as_ref());
        let (ports, verifier_started) = CoordinatedApprovalPorts::new(VerifierBehavior::Block, 300);
        let ports = Arc::new(ports);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[13; 32],
                200,
                ports.as_ref(),
            )
            .expect("challenge");

        let approval_manager = Arc::clone(&manager);
        let approval_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let approval = thread::spawn(move || {
            approval_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                approval_ports.as_ref(),
            )
        });
        verifier_started
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("verifier call entered");

        ports.set_current_time(60_100);
        assert_eq!(
            manager.poll(&start.device_code, 60_100),
            Err(DeviceAuthorizationError::Expired)
        );
        ports.release_verifier();
        assert_eq!(
            approval.join().expect("approval thread"),
            Err(DeviceAuthorizationError::Expired)
        );
        assert_eq!(ports.issuer_calls(), 0);
    }

    #[test]
    fn slow_verifier_cannot_reserve_issuance_after_ttl_without_an_expiry_poll() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = Arc::new(manager_with_store(store.clone()));
        let start = start(manager.as_ref());
        let (ports, verifier_started) = CoordinatedApprovalPorts::new(VerifierBehavior::Block, 300);
        let ports = Arc::new(ports);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[14; 32],
                200,
                ports.as_ref(),
            )
            .expect("challenge");

        let approval_manager = Arc::clone(&manager);
        let approval_ports = Arc::clone(&ports);
        let approval_id = challenge.approval_id;
        let approval = thread::spawn(move || {
            approval_manager.complete_approval(
                &approval_id,
                b"test-only-valid-assertion",
                300,
                approval_ports.as_ref(),
            )
        });
        verifier_started
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("verifier call entered");

        // No poll or denial performs expiry; the trusted clock still blocks
        // the Verifying -> Issuing transition after the verifier returns.
        ports.set_current_time(60_100);
        ports.release_verifier();
        assert_eq!(
            approval.join().expect("approval thread"),
            Err(DeviceAuthorizationError::Expired)
        );
        assert_eq!(ports.issuer_calls(), 0);
        assert!(matches!(
            store
                .by_device_code_hash(&hash_code(
                    b"device",
                    start.device_code.to_ascii_lowercase().as_bytes(),
                ))
                .expect("read expired state")
                .expect("authorization")
                .state,
            DeviceAuthorizationState::Expired
        ));
    }

    #[test]
    fn retrying_the_same_assertion_consumes_the_credential_counter_once() {
        let manager = manager();
        let start = start(&manager);
        let (ports, _verifier_started) =
            CoordinatedApprovalPorts::new(VerifierBehavior::UnavailableOnce, 300);
        let challenge = manager
            .begin_approval(&start.user_code, &scope(), &user(), &[15; 32], 200, &ports)
            .expect("challenge");

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::WebAuthnUnavailable)
        );
        assert!(matches!(
            manager
                .store
                .by_approval_id(&challenge.approval_id)
                .expect("read verifying state")
                .expect("authorization")
                .state,
            DeviceAuthorizationState::VerifyingWebAuthn { .. }
        ));
        assert_eq!(ports.credential_counter_updates(), 1);

        ports.set_current_time(301);
        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                301,
                &ports,
            ),
            Ok(())
        );
        assert_eq!(ports.verifier_calls(), 2);
        assert_eq!(ports.credential_counter_updates(), 1);
        assert_eq!(ports.issuer_calls(), 1);
    }
}
