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

/// Opaque server-generated identifier for one enrollment.
pub type DeviceAuthorizationId = [u8; 16];

/// Opaque digest stored in place of a one-time device or user code.
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

/// A user-facing WebAuthn challenge bound to one pending enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceApprovalChallenge {
    pub approval_id: DeviceAuthorizationId,
    /// Raw challenge bytes for the relying-party adapter to encode as needed.
    pub challenge: [u8; 32],
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
        challenge: [u8; 32],
    },
    Approved {
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
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizationRecord {
    pub id: DeviceAuthorizationId,
    pub device_code_hash: DeviceAuthorizationCodeHash,
    pub user_code_hash: DeviceAuthorizationCodeHash,
    pub scope: DeviceAuthorizationScope,
    pub csr_der: Vec<u8>,
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
    fn by_user_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
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
                || existing.user_code_hash == record.user_code_hash
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

    fn by_user_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| &record.user_code_hash == code_hash)
    }

    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| {
            matches!(
                record.state,
                DeviceAuthorizationState::AwaitingWebAuthn { approval_id: current, .. }
                    if &current == approval_id
            )
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
            || current.user_code_hash != replacement.user_code_hash
            || current.scope != replacement.scope
            || current.csr_der != replacement.csr_der
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

/// WebAuthn verifier for one approval assertion.
///
/// Production implementations must validate the signature, relying-party ID,
/// origin, user handle, credential status, and counter policy against the
/// trusted identity supplied here.
pub trait WebAuthnAssertionVerifier: Send + Sync {
    fn verify_approval_assertion(
        &self,
        approver: &UserIdentityRef,
        challenge: &[u8; 32],
        assertion: &[u8],
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationPortError>;
}

/// External CA boundary. Implementations must be idempotent by enrollment ID,
/// honor the exact organization/workspace scope, and bind the issued
/// certificate to the supplied SPKI digest.
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

/// Aggregate implemented by the approval orchestration layer so a completion
/// call can invoke all security checks through one explicit port bundle.
pub trait DeviceApprovalPorts:
    WorkspaceMembershipPort + DeviceCsrValidator + WebAuthnAssertionVerifier + DeviceCertificateIssuer
{
}

impl<T> DeviceApprovalPorts for T where
    T: WorkspaceMembershipPort
        + DeviceCsrValidator
        + WebAuthnAssertionVerifier
        + DeviceCertificateIssuer
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
    #[error("WebAuthn verifier unavailable")]
    WebAuthnUnavailable,
    #[error("certificate signer unavailable or rejected the request")]
    CertificateSigningFailed,
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
    policy: DeviceAuthorizationPolicy,
}

impl<S, L> DeviceAuthorizationManager<S, L>
where
    S: DeviceAuthorizationStore,
    L: UserCodeAttemptLimiter,
{
    /// Creates a state machine with validated local timing and rate limits.
    pub fn new(
        store: S,
        limiter: L,
        policy: DeviceAuthorizationPolicy,
    ) -> Result<Self, DeviceAuthorizationError> {
        if !policy.is_valid() {
            return Err(DeviceAuthorizationError::InvalidPolicy);
        }
        Ok(Self {
            store,
            limiter,
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
                user_code_hash: hash_code(b"user", normalize_user_code(&user_code).as_bytes()),
                scope: request.scope.clone(),
                csr_der: request.csr_der.clone(),
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
        membership: &impl WorkspaceMembershipPort,
    ) -> Result<DeviceApprovalChallenge, DeviceAuthorizationError> {
        self.record_user_code_attempt(abuse_key, now_unix_ms)?;
        validate_scope(scope)?;
        validate_identity(approver)?;
        let user_code = normalize_user_code(user_code);
        if !is_valid_user_code(&user_code) {
            return Err(DeviceAuthorizationError::InvalidCode);
        }
        let code_hash = hash_code(b"user", user_code.as_bytes());
        let mut record = self
            .store
            .by_user_code_hash(&code_hash)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        self.expire_if_due(&mut record, now_unix_ms)?;
        if record.scope != *scope {
            return Err(DeviceAuthorizationError::ScopeMismatch);
        }
        if !matches!(record.state, DeviceAuthorizationState::Pending) {
            return Err(state_error(&record.state));
        }
        match membership.is_member(approver, scope) {
            Ok(true) => {}
            Ok(false) => return Err(DeviceAuthorizationError::MembershipRequired),
            Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::MembershipRequired)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::MembershipUnavailable)
            }
        }

        let challenge = DeviceApprovalChallenge {
            approval_id: random_array::<16>()?,
            challenge: random_array::<32>()?,
        };
        record.state = DeviceAuthorizationState::AwaitingWebAuthn {
            approval_id: challenge.approval_id,
            approver: approver.clone(),
            challenge: challenge.challenge,
        };
        self.replace(record)?;
        Ok(challenge)
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
        if assertion.is_empty() || assertion.len() > 16 * 1024 {
            return Err(DeviceAuthorizationError::InvalidWebAuthnAssertion);
        }
        let mut record = self
            .store
            .by_approval_id(approval_id)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        self.expire_if_due(&mut record, now_unix_ms)?;
        let (approver, challenge) = match &record.state {
            DeviceAuthorizationState::AwaitingWebAuthn {
                approval_id: current,
                approver,
                challenge,
            } if current == approval_id => (approver.clone(), *challenge),
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

        match ports.verify_approval_assertion(&approver, &challenge, assertion, now_unix_ms) {
            Ok(()) => {}
            Err(DeviceAuthorizationPortError::Rejected) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::InvalidWebAuthnAssertion);
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::WebAuthnUnavailable);
            }
        }

        let certificate = match ports.issue_device_certificate(
            &record.id,
            &record.scope,
            &record.csr_der,
            &record.spki_sha256,
            now_unix_ms,
        ) {
            Ok(certificate) => certificate,
            Err(_) => {
                self.release_approval(record, approval_id)?;
                return Err(DeviceAuthorizationError::CertificateSigningFailed);
            }
        };
        if certificate.certificate_der.is_empty()
            || certificate.serial_number.is_empty()
            || certificate.scope != record.scope
            || certificate.spki_sha256 != record.spki_sha256
            || certificate.not_after_unix_ms <= now_unix_ms
        {
            self.deny_reserved_approval(record, approval_id, &approver, now_unix_ms)?;
            return Err(DeviceAuthorizationError::CertificateBindingMismatch);
        }

        record.state = DeviceAuthorizationState::Approved {
            approver,
            decided_at_unix_ms: now_unix_ms,
            certificate,
        };
        self.replace(record)
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
        let code_hash = hash_code(b"user", user_code.as_bytes());
        let mut record = self
            .store
            .by_user_code_hash(&code_hash)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        self.expire_if_due(&mut record, now_unix_ms)?;
        if record.scope != *scope {
            return Err(DeviceAuthorizationError::ScopeMismatch);
        }
        if !matches!(
            record.state,
            DeviceAuthorizationState::Pending | DeviceAuthorizationState::AwaitingWebAuthn { .. }
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

    /// Polls with the device code. Successful certificate delivery atomically
    /// consumes the enrollment, so replay cannot receive it a second time.
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
            | DeviceAuthorizationState::AwaitingWebAuthn { .. } => {
                let interval_ms = record.poll_interval_ms;
                self.replace(record)?;
                Ok(DeviceAuthorizationPoll::Pending { interval_ms })
            }
            DeviceAuthorizationState::Approved {
                approver,
                decided_at_unix_ms,
                certificate,
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

    fn expire_if_due(
        &self,
        record: &mut DeviceAuthorizationRecord,
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationError> {
        if now_unix_ms < record.expires_at_unix_ms {
            return Ok(());
        }
        match record.state {
            DeviceAuthorizationState::Pending
            | DeviceAuthorizationState::AwaitingWebAuthn { .. }
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
    let mut hasher = Sha256::new();
    hasher.update(b"cyrene-device-authorization-v1\0");
    hasher.update(kind);
    hasher.update(b"\0");
    hasher.update(code);
    hasher.finalize().into()
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCsrValidator;

    impl DeviceCsrValidator for TestCsrValidator {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok([7; 32])
        }
    }

    struct TestMembership(bool);

    impl WorkspaceMembershipPort for TestMembership {
        fn is_member(
            &self,
            _approver: &UserIdentityRef,
            _scope: &DeviceAuthorizationScope,
        ) -> Result<bool, DeviceAuthorizationPortError> {
            Ok(self.0)
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

    impl WebAuthnAssertionVerifier for TestApprovalPorts {
        fn verify_approval_assertion(
            &self,
            _approver: &UserIdentityRef,
            _challenge: &[u8; 32],
            assertion: &[u8],
            _now_unix_ms: u64,
        ) -> Result<(), DeviceAuthorizationPortError> {
            if assertion == b"test-only-valid-assertion" {
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

    fn manager(
    ) -> DeviceAuthorizationManager<InMemoryDeviceAuthorizationStore, InMemoryUserCodeAttemptLimiter>
    {
        DeviceAuthorizationManager::new(
            InMemoryDeviceAuthorizationStore::default(),
            InMemoryUserCodeAttemptLimiter::default(),
            DeviceAuthorizationPolicy {
                authorization_ttl_ms: 60_000,
                initial_poll_interval_ms: 1_000,
                maximum_poll_interval_ms: 5_000,
                slow_down_increment_ms: 1_000,
                user_code_attempt_window_ms: 60_000,
                maximum_user_code_attempts: 3,
            },
        )
        .expect("valid test policy")
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
                &TestMembership(true),
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
                &TestMembership(true),
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
                &TestMembership(true),
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
                &TestMembership(true),
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
                &TestMembership(true),
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
                &TestMembership(true),
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
                &TestMembership(true),
            )
            .expect("first denial");

        assert_eq!(
            manager.deny(
                &start.user_code,
                &scope(),
                &user(),
                &[6; 32],
                201,
                &TestMembership(true),
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
                &TestMembership(true),
            ),
            Err(DeviceAuthorizationError::Denied)
        );
        assert_eq!(
            manager.poll(&start.device_code, 203),
            Err(DeviceAuthorizationError::Denied)
        );
    }
}
