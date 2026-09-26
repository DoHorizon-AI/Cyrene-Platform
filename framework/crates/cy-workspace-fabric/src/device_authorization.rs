//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_authorization.rs                                         │
//! │  Module: cy_workspace_fabric::device_authorization                  │
//! │  Role: RFC 8628-style device enrollment state machine and ports.    │
//! │                                                                     │
//! │  模块职责：设备授权状态机及可替换的身份、CSR、存储和 CA 端口。           │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This module is a foundation layer, not a production Directory, WebAuthn,
//! registry, CA, or revocation implementation. Deployments must supply durable
//! storage and every external trust-boundary port before enabling the flow.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use cy_proto::workspace_v1::UserIdentityRef;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{UserCodeKeyRing, UserCodeSecretError, VersionedUserCodeDigest};

/// Opaque server-generated identifier for one authorization attempt.
///
/// This is the wire `authorization_id`; it is not a stable WorkspaceDevice ID.
pub type DeviceAuthorizationId = [u8; 16];

/// Maximum time a valid certificate may wait for device acknowledgement.
pub const MAX_CERTIFICATE_DELIVERY_TTL_MS: u64 = 5 * 60 * 1_000;

/// Opaque digest stored in place of the high-entropy one-time device code.
pub type DeviceAuthorizationCodeHash = [u8; 32];

const REGISTRATION_KEY_DOMAIN: &[u8] = b"cyrene-workspace-device-registration-key:v1\0";

/// Domain-separated digest of the client-generated registration recovery key.
/// The digest is safe to persist and never formats its bytes in diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceRegistrationKeyDigest([u8; 32]);

impl DeviceRegistrationKeyDigest {
    /// Derives the storage value from a validated, high-entropy recovery key.
    pub(crate) fn from_secret(secret: &[u8; 32]) -> Self {
        let mut digest = Sha256::new();
        digest.update(REGISTRATION_KEY_DOMAIN);
        digest.update(secret);
        Self(digest.finalize().into())
    }

    /// Reconstitutes a previously persisted digest without a raw credential.
    #[allow(dead_code)] // Used by the PostgreSQL codec added on the adapter branch.
    pub(crate) fn from_stored_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Exposes only the digest to the transactional storage adapter.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque device identity evidence obtained only from the verified mTLS peer.
/// It cannot be deserialized from an HTTP request or created by downstream
/// callers. Directory rotation adapters use it to authorize identity reuse.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedDeviceRotation {
    key: DeviceAuthorizationDeviceKey,
}

impl AuthenticatedDeviceRotation {
    /// Trusted in-crate verifier seam; callers must pass the certificate's
    /// already-verified WorkspaceDevice identity, never request JSON data.
    #[allow(dead_code)] // Wired by the verified mTLS host adapter.
    pub(crate) fn from_verified_mtls_peer(key: DeviceAuthorizationDeviceKey) -> Self {
        Self { key }
    }

    pub fn key(&self) -> &DeviceAuthorizationDeviceKey {
        &self.key
    }
}

impl fmt::Debug for DeviceRegistrationKeyDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceRegistrationKeyDigest([REDACTED])")
    }
}

/// Exact organization and workspace scope authorized for an enrolled device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationScope {
    pub organization_id: String,
    pub workspace_id: String,
}

/// Stable Directory device identity copied into one authorization record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationDeviceKey {
    pub organization_id: String,
    pub workspace_id: String,
    pub device_id: String,
}

/// Opaque, immutable snapshot of a Directory-authoritative registration.
///
/// Fields are private so request adapters cannot deserialize or manufacture an
/// identity. The only production conversion seam accepts the private trusted
/// Directory-result trait implemented inside this crate; tests use a cfg-only
/// fixture. Until that adapter and the registered-store transaction are wired,
/// `DeviceAuthorizationStore` defaults deny start and state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationRegistrationBinding {
    binding_id: [u8; 16],
    key: DeviceAuthorizationDeviceKey,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
}

/// Implemented only by the Directory authority adapter for its trusted result.
/// This crate-private seam prevents public request data from supplying identity.
#[allow(dead_code)] // The production Directory adapter is not integrated yet.
pub(crate) trait VerifiedDirectoryRegistrationBinding {
    fn binding_id(&self) -> &[u8; 16];
    fn organization_id(&self) -> &str;
    fn workspace_id(&self) -> &str;
    fn device_id(&self) -> &str;
    fn authorization_generation(&self) -> u64;
    fn csr_sha256(&self) -> &[u8; 32];
    fn spki_sha256(&self) -> &[u8; 32];
}

impl DeviceAuthorizationRegistrationBinding {
    #[allow(dead_code)] // Wired by the production Directory adapter at integration.
    pub(crate) fn from_verified_directory_binding(
        binding: &impl VerifiedDirectoryRegistrationBinding,
    ) -> Self {
        Self {
            binding_id: *binding.binding_id(),
            key: DeviceAuthorizationDeviceKey {
                organization_id: binding.organization_id().to_owned(),
                workspace_id: binding.workspace_id().to_owned(),
                device_id: binding.device_id().to_owned(),
            },
            authorization_generation: binding.authorization_generation(),
            csr_sha256: *binding.csr_sha256(),
            spki_sha256: *binding.spki_sha256(),
        }
    }

    pub(crate) fn binding_id(&self) -> &[u8; 16] {
        &self.binding_id
    }

    pub(crate) fn key(&self) -> &DeviceAuthorizationDeviceKey {
        &self.key
    }

    pub(crate) fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    pub(crate) fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    pub(crate) fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }

    #[cfg(test)]
    pub(crate) fn test_fixture(
        binding_id: [u8; 16],
        key: DeviceAuthorizationDeviceKey,
        authorization_generation: u64,
        csr_sha256: [u8; 32],
        spki_sha256: [u8; 32],
    ) -> Self {
        Self {
            binding_id,
            key,
            authorization_generation,
            csr_sha256,
            spki_sha256,
        }
    }
}

/// Input used to start one device enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationRequest {
    pub scope: DeviceAuthorizationScope,
    /// DER-encoded PKCS#10 CSR. Private key material must never be sent here.
    pub csr_der: Vec<u8>,
    /// Domain-separated digest of the 256-bit registration recovery key.
    pub registration_key_digest: DeviceRegistrationKeyDigest,
    /// Trusted result returned by the Directory registration authority.
    /// Request adapters must never construct or deserialize this value.
    pub registration_binding: DeviceAuthorizationRegistrationBinding,
}

/// Request accepted by the single-transaction Directory/authorization store.
/// The HTTP wire request cannot construct a Directory binding or rotation
/// identity; those are resolved from trusted server state inside the adapter.
pub struct DeviceAuthorizationRegistrationRequest {
    pub(crate) scope: DeviceAuthorizationScope,
    pub(crate) csr_der: Vec<u8>,
    pub(crate) csr_sha256: [u8; 32],
    pub(crate) spki_sha256: [u8; 32],
    pub(crate) registration_key_digest: DeviceRegistrationKeyDigest,
    pub(crate) authenticated_device: Option<AuthenticatedDeviceRotation>,
}

impl DeviceAuthorizationRegistrationRequest {
    pub(crate) fn new(
        scope: DeviceAuthorizationScope,
        csr_der: Vec<u8>,
        csr_sha256: [u8; 32],
        spki_sha256: [u8; 32],
        registration_key_digest: DeviceRegistrationKeyDigest,
    ) -> Self {
        Self {
            scope,
            csr_der,
            csr_sha256,
            spki_sha256,
            registration_key_digest,
            authenticated_device: None,
        }
    }

    #[allow(dead_code)] // Only the trusted mTLS composition may attach this marker.
    pub(crate) fn with_authenticated_device(
        mut self,
        authenticated_device: AuthenticatedDeviceRotation,
    ) -> Self {
        self.authenticated_device = Some(authenticated_device);
        self
    }

    pub fn scope(&self) -> &DeviceAuthorizationScope {
        &self.scope
    }

    pub fn csr_der(&self) -> &[u8] {
        &self.csr_der
    }

    pub fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    pub fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }

    pub fn registration_key_digest(&self) -> &DeviceRegistrationKeyDigest {
        &self.registration_key_digest
    }
}

/// Candidate passed to a single database transaction that resolves Directory
/// identity and starts or recovers the authorization. Candidate values are
/// generated by the manager; callers cannot choose IDs or code digests.
pub struct DeviceAuthorizationStartCandidate {
    scope: DeviceAuthorizationScope,
    csr_der: Vec<u8>,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    registration_key_digest: DeviceRegistrationKeyDigest,
    authorization_id_candidate: DeviceAuthorizationId,
    device_code_hash_candidate: DeviceAuthorizationCodeHash,
    user_code_digest_candidate: VersionedUserCodeDigest,
    authorization_ttl_ms: u64,
    initial_poll_interval_ms: u64,
    maximum_recovery_attempts: u32,
    observed_at_unix_ms: u64,
    authenticated_device: Option<AuthenticatedDeviceRotation>,
}

impl DeviceAuthorizationStartCandidate {
    pub fn scope(&self) -> &DeviceAuthorizationScope {
        &self.scope
    }

    pub fn csr_der(&self) -> &[u8] {
        &self.csr_der
    }

    pub fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    pub fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }

    pub fn registration_key_digest(&self) -> &DeviceRegistrationKeyDigest {
        &self.registration_key_digest
    }

    pub fn authorization_id_candidate(&self) -> &DeviceAuthorizationId {
        &self.authorization_id_candidate
    }

    pub fn device_code_hash_candidate(&self) -> &DeviceAuthorizationCodeHash {
        &self.device_code_hash_candidate
    }

    pub fn user_code_digest_candidate(&self) -> &VersionedUserCodeDigest {
        &self.user_code_digest_candidate
    }

    pub fn authorization_ttl_ms(&self) -> u64 {
        self.authorization_ttl_ms
    }

    pub fn initial_poll_interval_ms(&self) -> u64 {
        self.initial_poll_interval_ms
    }

    pub fn maximum_recovery_attempts(&self) -> u32 {
        self.maximum_recovery_attempts
    }

    /// Advisory only. Durable adapters must use database time for TTL and
    /// deadline decisions and return it in the committed snapshot.
    pub fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }

    pub fn authenticated_device(&self) -> Option<&AuthenticatedDeviceRotation> {
        self.authenticated_device.as_ref()
    }
}

/// Whether a composite registration transaction inserted a fresh authorization
/// or rotated the codes on an existing authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAuthorizationStartDisposition {
    Created,
    Recovered,
}

/// One committed projection of Directory binding and authorization state.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizationCommittedSnapshot {
    pub record: DeviceAuthorizationRecord,
    pub database_now_unix_ms: u64,
    pub disposition: DeviceAuthorizationStartDisposition,
}

/// One committed poll view. `record` carries the current Directory binding,
/// code generation, state, and row revision from the same database snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizationPollSnapshot {
    pub record: DeviceAuthorizationRecord,
    pub database_now_unix_ms: u64,
}

/// Codes and timing returned once to the device and its operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorizationStart {
    /// Stable ID for this authorization attempt and the CA idempotency key.
    pub authorization_id: DeviceAuthorizationId,
    pub device_id: String,
    pub authorization_generation: u64,
    /// Revision for same-authorization recovery code rotation.
    pub device_code_generation: u64,
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
    pub registration_binding_id: [u8; 16],
    pub device_key: DeviceAuthorizationDeviceKey,
    pub authorization_generation: u64,
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
    pub ca_chain_der: Vec<Vec<u8>>,
    pub serial_number: Vec<u8>,
    /// Issuer attestation for the exact Directory binding supplied to the port.
    pub registration_binding_id: [u8; 16],
    pub device_key: DeviceAuthorizationDeviceKey,
    pub authorization_generation: u64,
    pub scope: DeviceAuthorizationScope,
    pub spki_sha256: [u8; 32],
    pub not_after_unix_ms: u64,
}

/// Byte-identical certificate snapshot returned to the device until ACK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCertificateDelivery {
    pub authorization_id: DeviceAuthorizationId,
    pub delivery_id: DeviceAuthorizationId,
    pub device_id: String,
    pub authorization_generation: u64,
    pub certificate_der: Vec<u8>,
    pub ca_chain_der: Vec<Vec<u8>>,
    pub scope: DeviceAuthorizationScope,
    pub serial_number: Vec<u8>,
    pub not_after_unix_ms: u64,
    pub certificate_sha256: [u8; 32],
    pub csr_sha256: [u8; 32],
    pub csr_spki_sha256: [u8; 32],
    pub acknowledgement_deadline_unix_ms: u64,
}

/// Exact ACK receipt retained for idempotent retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCertificateDeliveryReceipt {
    pub authorization_id: DeviceAuthorizationId,
    pub delivery_id: DeviceAuthorizationId,
    pub device_id: String,
    pub authorization_generation: u64,
    pub certificate_sha256: [u8; 32],
    pub csr_sha256: [u8; 32],
    pub csr_spki_sha256: [u8; 32],
    pub acknowledged_at_unix_ms: u64,
}

/// Device-code authenticated acknowledgement of one certificate delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCertificateDeliveryAcknowledgement {
    pub authorization_id: DeviceAuthorizationId,
    pub device_code: String,
    pub delivery_id: DeviceAuthorizationId,
    pub certificate_sha256: [u8; 32],
    pub csr_sha256: [u8; 32],
    pub csr_spki_sha256: [u8; 32],
}

/// Why a certificate is quarantined pending retirement confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceCertificateRetirementReason {
    MisboundCertificate,
    DeliveryDeadlineReached,
    CertificateExpiredBeforeDelivery,
}

/// A definitive failure before a usable certificate became deliverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceCertificateIssuanceFailure {
    SignerRejectedWithoutCommit,
    MisboundCertificateRetired,
    CertificateExpiredBeforeDeliveryRetired,
}

/// Status exposed while certificate retirement is unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceDeliveryRecoveryStatus {
    RevocationPending,
    RecoveryBlocked,
}

/// Outcome of one manager-owned delivery retirement attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceCertificateRetirementWorkOutcome {
    /// The record is missing, not due, or no longer needs retirement.
    NoWork,
    /// The CA confirmed retirement and the state is terminal.
    Retired,
    /// Retirement is still durable and must be retried later.
    Pending(DeviceDeliveryRecoveryStatus),
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
    DeliveryPending {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        decided_at_unix_ms: u64,
        certificate: IssuedDeviceCertificate,
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        delivery_deadline_unix_ms: u64,
    },
    Delivered {
        approval_id: DeviceAuthorizationId,
        receipt: DeviceCertificateDeliveryReceipt,
    },
    RetirementPending {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        certificate: IssuedDeviceCertificate,
        certificate_sha256: [u8; 32],
        delivery_id: Option<DeviceAuthorizationId>,
        reason: DeviceCertificateRetirementReason,
        entered_at_unix_ms: u64,
        last_failure: Option<DeviceCertificateRetirementError>,
    },
    DeliveryExpired {
        approval_id: DeviceAuthorizationId,
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        expired_at_unix_ms: u64,
    },
    IssuanceFailed {
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        failed_at_unix_ms: u64,
        failure: DeviceCertificateIssuanceFailure,
        certificate_sha256: Option<[u8; 32]>,
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
    /// Immutable Directory binding for this exact authorization generation.
    pub registration_binding: DeviceAuthorizationRegistrationBinding,
    /// Domain-separated digest used for exact-tuple lost-start recovery.
    /// `None` is reserved for rows created before v4; such records cannot be
    /// recovered by registration key or used as a rotation authority.
    pub registration_key_digest: Option<DeviceRegistrationKeyDigest>,
    /// Monotonic code rotation revision, independent from device generation.
    pub device_code_generation: u64,
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
    CertificateReady(DeviceCertificateDelivery),
    Delivered(DeviceCertificateDeliveryReceipt),
    DeliveryExpired {
        delivery_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
    },
    RecoveryRequired {
        status: DeviceDeliveryRecoveryStatus,
    },
    IssuanceFailed {
        failure: DeviceCertificateIssuanceFailure,
    },
}

/// Poll outcome plus the exact committed authorization projection that backs
/// the outcome. HTTP adapters use the projection to reject mismatched metadata
/// and do not perform independent Directory or authorization reads.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizationPollResult {
    pub response: DeviceAuthorizationPoll,
    pub snapshot: DeviceAuthorizationPollSnapshot,
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
    pub maximum_recovery_attempts: u32,
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
            maximum_recovery_attempts: 5,
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
            && self.maximum_recovery_attempts > 0
            && self.maximum_recovery_attempts <= 10
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
    /// Creates a record only when the current Directory binding generation is
    /// checked and locked in the same transaction as authorization insertion.
    /// The default deliberately fails closed for stores without that composite
    /// transaction; legacy `insert` implementations are not an authorization path.
    fn insert_registered(
        &self,
        _record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    /// Atomically resolves the registration-key digest and Directory identity,
    /// then inserts or safely recovers the authorization and current codes.
    /// The binding, state, revision, and code generation returned here must be
    /// one committed database snapshot. Durable stores use database time, lock
    /// the registration key/binding and authorization rows, preserve all CA
    /// and delivery state on recovery, and reject tuple changes or terminal
    /// states. The default keeps production start/recovery unavailable.
    fn start_or_recover_registered(
        &self,
        _candidate: DeviceAuthorizationStartCandidate,
    ) -> Result<DeviceAuthorizationCommittedSnapshot, DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    /// Verifies the record revision and Directory generation against one
    /// current snapshot. The default denies use by legacy stores.
    fn require_current_registered_record(
        &self,
        _record: &DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    fn by_device_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    /// Reads the current record and Directory binding for a current device
    /// code in one snapshot. `expected_revision = None` performs the initial
    /// lookup; `Some(revision)` is the final stale-code fence after a state CAS.
    /// `observed_at_unix_ms` is only a local/test fallback; durable adapters
    /// must supply database time. The default is unavailable so legacy
    /// code-hash lookups cannot serve production polls.
    fn current_poll_snapshot(
        &self,
        _code_hash: &DeviceAuthorizationCodeHash,
        _expected_revision: Option<u64>,
        _observed_at_unix_ms: u64,
    ) -> Result<Option<DeviceAuthorizationPollSnapshot>, DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    fn by_user_code_candidates(
        &self,
        candidates: &[VersionedUserCodeDigest],
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    /// Returns every key version still referenced by a stored record.
    ///
    /// Startup must pass these versions to [`UserCodeKeyRing::require_versions`]
    /// before serving user-code lookups.
    fn user_code_key_versions(&self) -> Result<Vec<u32>, DeviceAuthorizationStoreError>;
    /// Resolves an approval through active, delivery, retirement, and terminal
    /// states so interrupted verifier/issuer work can be resumed safely.
    fn by_approval_id(
        &self,
        approval_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError>;
    /// Resolves one authorization by its non-secret, server-generated ID.
    ///
    /// The default is unavailable so legacy adapters cannot accidentally
    /// become background retirement stores.
    fn by_authorization_id(
        &self,
        _authorization_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError>;
    /// Atomically reserves a due `DeliveryPending` record for retirement.
    ///
    /// Durable implementations must verify the exact stored revision and
    /// state, and verify either the delivery deadline or certificate expiry
    /// against database time in the same transaction before writing
    /// `RetirementPending`. The default deliberately fails closed for stores
    /// without that database-time CAS.
    fn compare_and_swap_due_delivery_to_retirement(
        &self,
        _expected_revision: u64,
        _replacement: DeviceAuthorizationRecord,
    ) -> Result<bool, DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    /// Applies an authorization transition only while the immutable Directory
    /// binding and current generation are checked under the same database
    /// transaction/row lock. The default denies use by legacy stores.
    fn compare_and_swap_registered(
        &self,
        _expected_revision: u64,
        _replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
    /// Commits `DeliveryPending -> Delivered` only if the current stored
    /// deadline is still open. Durable adapters must check database time and
    /// perform this transition in one transaction; caller time alone is not
    /// an atomic deadline guard.
    fn compare_and_swap_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError>;
    /// Commits a new ACK only when both the persisted deadline and current
    /// Directory generation are checked in one transaction. Exact replay of
    /// an already committed receipt is handled before this method is called.
    fn compare_and_swap_registered_delivery_ack(
        &self,
        _expected_revision: u64,
        _replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        Err(DeviceAuthorizationStoreError::Unavailable)
    }
}

/// Storage failures exposed by the authorization state machine.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeviceAuthorizationStoreError {
    #[error("device authorization code collision")]
    CodeCollision,
    #[error("device authorization changed concurrently")]
    Conflict,
    #[error("device authorization recovery attempts exceeded")]
    RecoveryLimit,
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
                || (existing.registration_key_digest.is_some()
                    && existing.registration_key_digest == record.registration_key_digest)
                || existing.id == record.id
        }) {
            return Err(DeviceAuthorizationStoreError::CodeCollision);
        }
        records.insert(record.id, record);
        Ok(())
    }

    fn insert_registered(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let key = record.registration_binding.key();
        let generation = record.registration_binding.authorization_generation();
        let newest_generation = records
            .values()
            .filter(|existing| existing.registration_binding.key() == key)
            .map(|existing| existing.registration_binding.authorization_generation())
            .max();
        if newest_generation.is_some_and(|newest| generation < newest) {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        if newest_generation.is_some_and(|newest| generation > newest)
            && records.values().any(|existing| {
                existing.registration_binding.key() == key
                    && newest_generation
                        == Some(existing.registration_binding.authorization_generation())
                    && matches!(
                        &existing.state,
                        DeviceAuthorizationState::Issuing { .. }
                            | DeviceAuthorizationState::DeliveryPending { .. }
                            | DeviceAuthorizationState::RetirementPending { .. }
                    )
            })
        {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        if records.values().any(|existing| {
            existing.device_code_hash == record.device_code_hash
                || existing.user_code_digest == record.user_code_digest
                || (existing.registration_key_digest.is_some()
                    && existing.registration_key_digest == record.registration_key_digest)
                || existing.id == record.id
                || existing.registration_binding.binding_id()
                    == record.registration_binding.binding_id()
                || (existing.registration_binding.key() == key
                    && existing.registration_binding.authorization_generation() == generation)
        }) {
            return Err(DeviceAuthorizationStoreError::CodeCollision);
        }
        records.insert(record.id, record);
        Ok(())
    }

    fn require_current_registered_record(
        &self,
        record: &DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let current = records
            .get(&record.id)
            .ok_or(DeviceAuthorizationStoreError::Conflict)?;
        if current.revision != record.revision
            || current.registration_binding != record.registration_binding
        {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        let newest_generation = records
            .values()
            .filter(|existing| {
                existing.registration_binding.key() == record.registration_binding.key()
            })
            .map(|existing| existing.registration_binding.authorization_generation())
            .max()
            .ok_or(DeviceAuthorizationStoreError::Conflict)?;
        if newest_generation != record.registration_binding.authorization_generation() {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        Ok(())
    }

    fn by_device_code_hash(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| &record.device_code_hash == code_hash)
    }

    fn current_poll_snapshot(
        &self,
        code_hash: &DeviceAuthorizationCodeHash,
        expected_revision: Option<u64>,
        observed_at_unix_ms: u64,
    ) -> Result<Option<DeviceAuthorizationPollSnapshot>, DeviceAuthorizationStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let Some(record) = records
            .values()
            .find(|record| &record.device_code_hash == code_hash)
        else {
            return Ok(None);
        };
        if expected_revision.is_some_and(|revision| record.revision != revision) {
            return Ok(None);
        }
        Ok(Some(DeviceAuthorizationPollSnapshot {
            record: record.clone(),
            database_now_unix_ms: observed_at_unix_ms,
        }))
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
            | DeviceAuthorizationState::DeliveryPending {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::RetirementPending {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::IssuanceFailed {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::Delivered {
                approval_id: current,
                ..
            }
            | DeviceAuthorizationState::DeliveryExpired {
                approval_id: current,
                ..
            } => current == approval_id,
            _ => false,
        })
    }

    fn by_authorization_id(
        &self,
        authorization_id: &DeviceAuthorizationId,
    ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
        self.find(|record| &record.id == authorization_id)
    }

    fn compare_and_swap(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        self.compare_and_swap_inner(expected_revision, replacement, false, false)
    }

    fn compare_and_swap_due_delivery_to_retirement(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<bool, DeviceAuthorizationStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let Some(current) = records.get(&replacement.id) else {
            return Ok(false);
        };
        if current.revision != expected_revision {
            return Ok(false);
        }
        if !matches!(
            &current.state,
            DeviceAuthorizationState::DeliveryPending { .. }
        ) {
            return Ok(false);
        }
        if !matches!(
            &replacement.state,
            DeviceAuthorizationState::RetirementPending {
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                last_failure: None,
                ..
            }
        ) {
            return Ok(false);
        }
        let mut expected = current.clone();
        expected.revision = expected_revision
            .checked_add(1)
            .ok_or(DeviceAuthorizationStoreError::Conflict)?;
        expected.state = replacement.state.clone();
        if replacement != expected {
            return Ok(false);
        }
        records.insert(replacement.id, replacement);
        Ok(true)
    }

    fn compare_and_swap_registered(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        self.compare_and_swap_inner(expected_revision, replacement, false, true)
    }

    fn compare_and_swap_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        self.compare_and_swap_inner(expected_revision, replacement, true, false)
    }

    fn compare_and_swap_registered_delivery_ack(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        self.compare_and_swap_inner(expected_revision, replacement, true, true)
    }
}

impl InMemoryDeviceAuthorizationStore {
    fn compare_and_swap_inner(
        &self,
        expected_revision: u64,
        replacement: DeviceAuthorizationRecord,
        delivery_ack: bool,
        registered: bool,
    ) -> Result<(), DeviceAuthorizationStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| DeviceAuthorizationStoreError::Unavailable)?;
        let current = records
            .get(&replacement.id)
            .ok_or(DeviceAuthorizationStoreError::Conflict)?;
        if registered {
            let key = replacement.registration_binding.key();
            let generation = replacement.registration_binding.authorization_generation();
            let newest_generation = records
                .values()
                .filter(|existing| existing.registration_binding.key() == key)
                .map(|existing| existing.registration_binding.authorization_generation())
                .max()
                .ok_or(DeviceAuthorizationStoreError::Conflict)?;
            if generation != newest_generation {
                return Err(DeviceAuthorizationStoreError::Conflict);
            }
        }
        let Some(next_revision) = expected_revision.checked_add(1) else {
            return Err(DeviceAuthorizationStoreError::Conflict);
        };
        if current.revision != expected_revision
            || replacement.revision != next_revision
            || current.registration_key_digest != replacement.registration_key_digest
            || current.device_code_generation != replacement.device_code_generation
            || current.device_code_hash != replacement.device_code_hash
            || current.user_code_digest != replacement.user_code_digest
            || current.scope != replacement.scope
            || current.csr_der != replacement.csr_der
            || current.csr_sha256 != replacement.csr_sha256
            || current.spki_sha256 != replacement.spki_sha256
            || current.created_at_unix_ms != replacement.created_at_unix_ms
            || current.expires_at_unix_ms != replacement.expires_at_unix_ms
            || current.registration_binding != replacement.registration_binding
        {
            return Err(DeviceAuthorizationStoreError::Conflict);
        }
        if delivery_ack {
            let valid_ack_transition = match (&current.state, &replacement.state) {
                (
                    DeviceAuthorizationState::DeliveryPending {
                        approval_id: current_approval_id,
                        delivery_id: current_delivery_id,
                        certificate_sha256: current_certificate_sha256,
                        delivery_deadline_unix_ms,
                        ..
                    },
                    DeviceAuthorizationState::Delivered {
                        approval_id: next_approval_id,
                        receipt,
                    },
                ) => {
                    current_approval_id == next_approval_id
                        && receipt.authorization_id == replacement.id
                        && current_delivery_id == &receipt.delivery_id
                        && current_certificate_sha256 == &receipt.certificate_sha256
                        && receipt.csr_sha256 == current.csr_sha256
                        && receipt.csr_spki_sha256 == current.spki_sha256
                        && receipt.device_id == current.registration_binding.key().device_id
                        && receipt.authorization_generation
                            == current.registration_binding.authorization_generation()
                        && receipt.acknowledged_at_unix_ms < *delivery_deadline_unix_ms
                }
                _ => false,
            };
            if !valid_ack_transition {
                return Err(DeviceAuthorizationStoreError::Conflict);
            }
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
/// deduplicate by authorization ID, returning the same certificate for every
/// retry of the same Directory registration binding, CSR, and `issued_at`
/// value, including after a signer restart. The result must attest the exact
/// binding ID, stable device key, authorization generation, scope, and SPKI.
/// A definitive rejection must also prove no concurrent request for that ID
/// can later commit; every ambiguous result must use `OutcomeUnknown`. The
/// authorization record is durably reserved as `Issuing` before this port is
/// called.
pub trait DeviceCertificateIssuer: Send + Sync {
    fn issue_device_certificate(
        &self,
        authorization_id: &DeviceAuthorizationId,
        registration_binding: &DeviceAuthorizationRegistrationBinding,
        csr_der: &[u8],
        issued_at_unix_ms: u64,
    ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError>;
}

/// Signer result distinguishes definite no-commit from an ambiguous outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceCertificateIssuanceError {
    /// The signer guarantees that no certificate was or can be committed for
    /// this authorization ID and exact request tuple.
    #[error("certificate request was definitively rejected without commit")]
    DefinitiveNoCommit,
    /// The signer may have committed; keep `Issuing` and reconcile by retrying
    /// the same authorization ID and exact inputs.
    #[error("certificate issuance outcome is unknown")]
    OutcomeUnknown,
}

/// External idempotent certificate retirement/revocation boundary.
///
/// `retire_or_confirm` must be durable and idempotent by authorization ID and
/// certificate fingerprint. Success means the certificate is unusable or was
/// already retired. Every error leaves the record in `RetirementPending` for
/// explicit retry or operator recovery; errors never authorize delivery.
pub trait DeviceCertificateRetirementPort: Send + Sync {
    fn retire_or_confirm(
        &self,
        authorization_id: &DeviceAuthorizationId,
        certificate_sha256: &[u8; 32],
        certificate: &IssuedDeviceCertificate,
        reason: DeviceCertificateRetirementReason,
    ) -> Result<(), DeviceCertificateRetirementError>;
}

/// Ports needed by device polling and certificate acknowledgement.
pub trait DeviceCertificateDeliveryPorts:
    DeviceCertificateRetirementPort + DeviceAuthorizationClockPort
{
}

impl<T> DeviceCertificateDeliveryPorts for T where
    T: DeviceCertificateRetirementPort + DeviceAuthorizationClockPort
{
}

/// Retirement outcomes. Both errors leave the certificate quarantined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceCertificateRetirementError {
    /// Retirement did not commit; operator or backend recovery is required.
    #[error("certificate retirement was definitively rejected")]
    Rejected,
    /// Retirement may have committed; retry the same authorization/fingerprint.
    #[error("certificate retirement outcome is unknown")]
    OutcomeUnknown,
}

impl DeviceCertificateRetirementError {
    fn recovery_status(self) -> DeviceDeliveryRecoveryStatus {
        match self {
            Self::Rejected => DeviceDeliveryRecoveryStatus::RecoveryBlocked,
            Self::OutcomeUnknown => DeviceDeliveryRecoveryStatus::RevocationPending,
        }
    }
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
    + DeviceCertificateDeliveryPorts
{
}

impl<T> DeviceApprovalPorts for T where
    T: DeviceApprovalStartPorts
        + DeviceCsrValidator
        + DeviceCertificateIssuer
        + DeviceCertificateDeliveryPorts
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
    #[error("Directory registration binding does not match the validated request")]
    InvalidRegistrationBinding,
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
    #[error("certificate retirement is unresolved: {status:?}")]
    CertificateRetirementPending {
        status: DeviceDeliveryRecoveryStatus,
    },
    #[error("certificate delivery acknowledgement does not match the pending delivery")]
    InvalidDeliveryAcknowledgement,
    #[error("certificate delivery has expired")]
    DeliveryExpired,
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
        if !registration_binding_matches(
            &request.registration_binding,
            &request.scope,
            &csr_sha256,
            &spki_sha256,
        ) {
            return Err(DeviceAuthorizationError::InvalidRegistrationBinding);
        }

        // A bounded retry handles the vanishingly unlikely random code collision.
        for _ in 0..3 {
            let enrollment_id = random_array::<16>()?;
            let device_code_bytes = random_array::<32>()?;
            let user_code_entropy = random_array::<8>()?;
            let device_code = encode_hex(&device_code_bytes);
            let user_code = encode_user_code(&user_code_entropy);
            let record = DeviceAuthorizationRecord {
                id: enrollment_id,
                registration_binding: request.registration_binding.clone(),
                registration_key_digest: Some(request.registration_key_digest.clone()),
                device_code_generation: 1,
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
            match self.store.insert_registered(record) {
                Ok(()) => {
                    return Ok(DeviceAuthorizationStart {
                        authorization_id: enrollment_id,
                        device_id: request.registration_binding.key().device_id.clone(),
                        authorization_generation: request
                            .registration_binding
                            .authorization_generation(),
                        device_code_generation: 1,
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

    /// Creates or recovers a registration through one atomic Directory and
    /// authorization store operation. The store returns the exact committed
    /// binding, state, revision, and code generation. A missing composite
    /// transaction is unavailable by default.
    pub fn begin_or_recover_registered(
        &self,
        request: DeviceAuthorizationRegistrationRequest,
        now_unix_ms: u64,
        csr_validator: &impl DeviceCsrValidator,
    ) -> Result<DeviceAuthorizationStart, DeviceAuthorizationError> {
        validate_scope(&request.scope)?;
        if request.csr_der.is_empty() || request.csr_der.len() > 16 * 1024 {
            return Err(DeviceAuthorizationError::InvalidRequest);
        }
        let csr_sha256 = sha256(&request.csr_der);
        if csr_sha256 != request.csr_sha256 {
            return Err(DeviceAuthorizationError::InvalidRequest);
        }
        let spki_sha256 = match csr_validator.validate_and_hash_spki(&request.csr_der) {
            Ok(hash) => hash,
            Err(DeviceAuthorizationPortError::Rejected) => {
                return Err(DeviceAuthorizationError::InvalidCsr)
            }
            Err(DeviceAuthorizationPortError::Unavailable) => {
                return Err(DeviceAuthorizationError::CsrValidatorUnavailable)
            }
        };
        if spki_sha256 != request.spki_sha256
            || request
                .registration_key_digest
                .as_bytes()
                .iter()
                .all(|byte| *byte == 0)
        {
            return Err(DeviceAuthorizationError::InvalidRequest);
        }

        for _ in 0..3 {
            let authorization_id_candidate = random_array::<16>()?;
            let device_code_bytes = random_array::<32>()?;
            let user_code_entropy = random_array::<8>()?;
            let device_code = encode_hex(&device_code_bytes);
            let user_code = encode_user_code(&user_code_entropy);
            let device_code_hash_candidate = hash_code(b"device", device_code.as_bytes());
            let user_code_digest_candidate = self
                .user_code_keys
                .digest_for_storage(&normalize_user_code(&user_code))
                .map_err(map_user_code_secret_error)?;
            let candidate = DeviceAuthorizationStartCandidate {
                scope: request.scope.clone(),
                csr_der: request.csr_der.clone(),
                csr_sha256,
                spki_sha256,
                registration_key_digest: request.registration_key_digest.clone(),
                authorization_id_candidate,
                device_code_hash_candidate,
                user_code_digest_candidate: user_code_digest_candidate.clone(),
                authorization_ttl_ms: self.policy.authorization_ttl_ms,
                initial_poll_interval_ms: self.policy.initial_poll_interval_ms,
                maximum_recovery_attempts: self.policy.maximum_recovery_attempts,
                observed_at_unix_ms: now_unix_ms,
                authenticated_device: request.authenticated_device.clone(),
            };
            match self.store.start_or_recover_registered(candidate) {
                Ok(snapshot) => {
                    let record = snapshot.record;
                    if record.registration_key_digest.as_ref()
                        != Some(&request.registration_key_digest)
                        || record.device_code_hash != device_code_hash_candidate
                        || record.user_code_digest != user_code_digest_candidate
                        || !registration_binding_matches(
                            &record.registration_binding,
                            &request.scope,
                            &csr_sha256,
                            &spki_sha256,
                        )
                        || record.scope != request.scope
                        || record.csr_der != request.csr_der
                        || record.csr_sha256 != csr_sha256
                        || record.spki_sha256 != spki_sha256
                        || record.device_code_generation == 0
                        || (snapshot.disposition == DeviceAuthorizationStartDisposition::Created
                            && (record.id != authorization_id_candidate
                                || record.device_code_generation != 1))
                        || (snapshot.disposition == DeviceAuthorizationStartDisposition::Recovered
                            && record.device_code_generation < 2)
                    {
                        return Err(DeviceAuthorizationError::StorageUnavailable);
                    }
                    return Ok(DeviceAuthorizationStart {
                        authorization_id: record.id,
                        device_id: record.registration_binding.key().device_id.clone(),
                        authorization_generation: record
                            .registration_binding
                            .authorization_generation(),
                        device_code_generation: record.device_code_generation,
                        device_code,
                        user_code,
                        expires_at_unix_ms: record.expires_at_unix_ms,
                        poll_interval_ms: record.poll_interval_ms,
                    });
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
        self.require_current_registration(&record)?;
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
            registration_binding_id: *record.registration_binding.binding_id(),
            device_key: record.registration_binding.key().clone(),
            authorization_generation: record.registration_binding.authorization_generation(),
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
        self.require_current_registration(&record)?;

        // An Issuing record proves that membership, CSR binding, and the
        // WebAuthn assertion were already checked before the durable CAS.
        // Resume using the persisted request parameters and enrollment ID;
        // re-verifying a one-time assertion here could prevent recovery.
        match &record.state {
            DeviceAuthorizationState::Issuing { .. } => {
                return self.issue_reserved_approval(record, ports);
            }
            DeviceAuthorizationState::DeliveryPending { .. }
            | DeviceAuthorizationState::Delivered { .. } => return Ok(()),
            DeviceAuthorizationState::RetirementPending { .. } => {
                return self.resume_retirement(record, ports)
            }
            DeviceAuthorizationState::DeliveryExpired { .. } => {
                return Err(DeviceAuthorizationError::DeliveryExpired)
            }
            DeviceAuthorizationState::IssuanceFailed { failure, .. } => {
                return Err(issuance_failure_error(*failure))
            }
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
            registration_binding_id: *record.registration_binding.binding_id(),
            device_key: record.registration_binding.key().clone(),
            authorization_generation: record.registration_binding.authorization_generation(),
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
        match record.state {
            DeviceAuthorizationState::Issuing { .. } => self.issue_reserved_approval(record, ports),
            DeviceAuthorizationState::DeliveryPending { .. }
            | DeviceAuthorizationState::Delivered { .. } => Ok(()),
            DeviceAuthorizationState::RetirementPending { .. } => {
                self.resume_retirement(record, ports)
            }
            DeviceAuthorizationState::DeliveryExpired { .. } => {
                Err(DeviceAuthorizationError::DeliveryExpired)
            }
            DeviceAuthorizationState::IssuanceFailed { failure, .. } => {
                Err(issuance_failure_error(failure))
            }
            state => Err(state_error(&state)),
        }
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
                DeviceAuthorizationState::DeliveryPending {
                    approval_id: current,
                    ..
                }
                | DeviceAuthorizationState::Delivered {
                    approval_id: current,
                    ..
                }
                | DeviceAuthorizationState::RetirementPending {
                    approval_id: current,
                    ..
                }
                | DeviceAuthorizationState::IssuanceFailed {
                    approval_id: current,
                    ..
                }
                | DeviceAuthorizationState::DeliveryExpired {
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
            &record.registration_binding,
            &record.csr_der,
            issued_at_unix_ms,
        ) {
            Ok(certificate) => certificate,
            Err(DeviceCertificateIssuanceError::OutcomeUnknown) => {
                // Keep Issuing so a later request can replay the same CA key.
                // If a concurrent retry completed, observe that committed state.
                let current = self
                    .store
                    .by_approval_id(&approval_id)
                    .map_err(map_store_error)?
                    .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                return match current.state {
                    DeviceAuthorizationState::Issuing { .. } => {
                        Err(DeviceAuthorizationError::CertificateIssuanceInProgress)
                    }
                    _ => self.completed_state_result(current),
                };
            }
            Err(DeviceCertificateIssuanceError::DefinitiveNoCommit) => {
                return self.fail_issuance(
                    record,
                    approval_id,
                    approver,
                    issued_at_unix_ms,
                    DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit,
                    None,
                )
            }
        };

        let certificate_sha256 = sha256(&certificate.certificate_der);
        if certificate.certificate_der.is_empty()
            || certificate.serial_number.is_empty()
            || certificate.registration_binding_id != *record.registration_binding.binding_id()
            || certificate.device_key != *record.registration_binding.key()
            || certificate.authorization_generation
                != record.registration_binding.authorization_generation()
            || certificate.scope != record.scope
            || certificate.spki_sha256 != *record.registration_binding.spki_sha256()
        {
            return self.quarantine_issued_certificate(
                record,
                DeviceAuthorizationState::RetirementPending {
                    approval_id,
                    approver,
                    certificate,
                    certificate_sha256,
                    delivery_id: None,
                    reason: DeviceCertificateRetirementReason::MisboundCertificate,
                    entered_at_unix_ms: issued_at_unix_ms,
                    last_failure: None,
                },
                ports,
            );
        }

        // The signer may return after the certificate's validity window has
        // ended. Never publish an already expired certificate.
        let delivery_started_at_unix_ms = ports
            .current_unix_ms()
            .map_err(|_| DeviceAuthorizationError::ClockUnavailable)?;
        if certificate.not_after_unix_ms <= issued_at_unix_ms
            || certificate.not_after_unix_ms <= delivery_started_at_unix_ms
        {
            return self.quarantine_issued_certificate(
                record,
                DeviceAuthorizationState::RetirementPending {
                    approval_id,
                    approver,
                    certificate,
                    certificate_sha256,
                    delivery_id: None,
                    reason: DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery,
                    entered_at_unix_ms: delivery_started_at_unix_ms,
                    last_failure: None,
                },
                ports,
            );
        }
        let delivery_deadline_unix_ms = delivery_started_at_unix_ms
            .saturating_add(MAX_CERTIFICATE_DELIVERY_TTL_MS)
            .min(certificate.not_after_unix_ms);
        if delivery_deadline_unix_ms <= delivery_started_at_unix_ms {
            return self.quarantine_issued_certificate(
                record,
                DeviceAuthorizationState::RetirementPending {
                    approval_id,
                    approver,
                    certificate,
                    certificate_sha256,
                    delivery_id: None,
                    reason: DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery,
                    entered_at_unix_ms: delivery_started_at_unix_ms,
                    last_failure: None,
                },
                ports,
            );
        }

        let delivery_id = random_array::<16>()?;
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::Issuing {
                    approval_id: current_id,
                    approver: current_approver,
                    issued_at_unix_ms: current_issued_at,
                } if current_id == &approval_id
                    && current_approver == &approver
                    && *current_issued_at == issued_at_unix_ms => {}
                DeviceAuthorizationState::DeliveryPending {
                    approval_id: current_id,
                    certificate_sha256: current_certificate_sha256,
                    ..
                }
                | DeviceAuthorizationState::Delivered {
                    approval_id: current_id,
                    receipt:
                        DeviceCertificateDeliveryReceipt {
                            certificate_sha256: current_certificate_sha256,
                            ..
                        },
                } if current_id == &approval_id
                    && current_certificate_sha256 == &certificate_sha256 =>
                {
                    return Ok(());
                }
                state => return Err(state_error(state)),
            }

            let mut replacement = record.clone();
            replacement.state = DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver: approver.clone(),
                decided_at_unix_ms: delivery_started_at_unix_ms,
                certificate: certificate.clone(),
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
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

    fn fail_issuance(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: DeviceAuthorizationId,
        approver: UserIdentityRef,
        failed_at_unix_ms: u64,
        failure: DeviceCertificateIssuanceFailure,
        certificate_sha256: Option<[u8; 32]>,
    ) -> Result<(), DeviceAuthorizationError> {
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::Issuing {
                    approval_id: current_id,
                    approver: current_approver,
                    ..
                } if current_id == &approval_id && current_approver == &approver => {}
                _ => return self.completed_state_result(record),
            }

            let mut replacement = record.clone();
            replacement.state = DeviceAuthorizationState::IssuanceFailed {
                approval_id,
                approver: approver.clone(),
                failed_at_unix_ms,
                failure,
                certificate_sha256,
            };
            match self.replace(replacement) {
                Ok(()) => return Err(issuance_failure_error(failure)),
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

    fn quarantine_issued_certificate(
        &self,
        record: DeviceAuthorizationRecord,
        retirement: DeviceAuthorizationState,
        ports: &impl DeviceApprovalPorts,
    ) -> Result<(), DeviceAuthorizationError> {
        let record = self.reserve_retirement(record, retirement)?;
        let record = self.try_retirement(record, ports)?;
        self.completed_state_result(record)
    }

    fn reserve_retirement(
        &self,
        mut record: DeviceAuthorizationRecord,
        retirement: DeviceAuthorizationState,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        for _ in 0..5 {
            if !retirement_transition_allowed(&record.state, &retirement) {
                return if retirement_state_already_resolved(&record.state, &retirement) {
                    Ok(record)
                } else {
                    Err(state_error(&record.state))
                };
            }
            let mut replacement = record.clone();
            replacement.state = retirement.clone();
            match self.replace(replacement) {
                Ok(()) => {
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    record.state = retirement;
                    return Ok(record);
                }
                Err(DeviceAuthorizationError::ConcurrentTransition) => {
                    let approval_id = state_approval_id(&retirement)
                        .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
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

    fn try_retirement(
        &self,
        record: DeviceAuthorizationRecord,
        retirement_port: &impl DeviceCertificateRetirementPort,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        let (approval_id, approver, certificate, certificate_sha256, delivery_id, reason) =
            match &record.state {
                DeviceAuthorizationState::RetirementPending {
                    approval_id,
                    approver,
                    certificate,
                    certificate_sha256,
                    delivery_id,
                    reason,
                    ..
                } => (
                    *approval_id,
                    approver.clone(),
                    certificate.clone(),
                    *certificate_sha256,
                    *delivery_id,
                    *reason,
                ),
                _ => return Err(state_error(&record.state)),
            };

        match retirement_port.retire_or_confirm(
            &record.id,
            &certificate_sha256,
            &certificate,
            reason,
        ) {
            Ok(()) => {
                let terminal = match reason {
                    DeviceCertificateRetirementReason::DeliveryDeadlineReached => {
                        DeviceAuthorizationState::DeliveryExpired {
                            approval_id,
                            delivery_id: delivery_id
                                .ok_or(DeviceAuthorizationError::ConcurrentTransition)?,
                            certificate_sha256,
                            expired_at_unix_ms: retirement_entered_at(&record.state),
                        }
                    }
                    DeviceCertificateRetirementReason::MisboundCertificate => {
                        DeviceAuthorizationState::IssuanceFailed {
                            approval_id,
                            approver: approver.clone(),
                            failed_at_unix_ms: retirement_entered_at(&record.state),
                            failure: DeviceCertificateIssuanceFailure::MisboundCertificateRetired,
                            certificate_sha256: Some(certificate_sha256),
                        }
                    }
                    DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery => {
                        DeviceAuthorizationState::IssuanceFailed {
                            approval_id,
                            approver: approver.clone(),
                            failed_at_unix_ms: retirement_entered_at(&record.state),
                            failure: DeviceCertificateIssuanceFailure::
                                CertificateExpiredBeforeDeliveryRetired,
                            certificate_sha256: Some(certificate_sha256),
                        }
                    }
                };
                self.commit_retirement_terminal(record, approval_id, certificate_sha256, terminal)
            }
            Err(error) => {
                self.persist_retirement_failure(record, approval_id, certificate_sha256, error)
            }
        }
    }

    fn commit_retirement_terminal(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        terminal: DeviceAuthorizationState,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::RetirementPending {
                    approval_id: current_id,
                    certificate_sha256: current_hash,
                    ..
                } if current_id == &approval_id && current_hash == &certificate_sha256 => {}
                DeviceAuthorizationState::DeliveryExpired {
                    approval_id: current_id,
                    certificate_sha256: current_hash,
                    ..
                }
                | DeviceAuthorizationState::IssuanceFailed {
                    approval_id: current_id,
                    certificate_sha256: Some(current_hash),
                    ..
                } if current_id == &approval_id && current_hash == &certificate_sha256 => {
                    return Ok(record)
                }
                state => return Err(state_error(state)),
            }
            let mut replacement = record.clone();
            replacement.state = terminal.clone();
            match self.replace_retirement(replacement) {
                Ok(()) => {
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    record.state = terminal;
                    return Ok(record);
                }
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

    fn persist_retirement_failure(
        &self,
        mut record: DeviceAuthorizationRecord,
        approval_id: DeviceAuthorizationId,
        certificate_sha256: [u8; 32],
        failure: DeviceCertificateRetirementError,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        for _ in 0..5 {
            match &mut record.state {
                DeviceAuthorizationState::RetirementPending {
                    approval_id: current_id,
                    certificate_sha256: current_hash,
                    last_failure,
                    ..
                } if current_id == &approval_id && current_hash == &certificate_sha256 => {
                    *last_failure = Some(failure);
                }
                DeviceAuthorizationState::DeliveryExpired {
                    approval_id: current_id,
                    certificate_sha256: current_hash,
                    ..
                }
                | DeviceAuthorizationState::IssuanceFailed {
                    approval_id: current_id,
                    certificate_sha256: Some(current_hash),
                    ..
                } if current_id == &approval_id && current_hash == &certificate_sha256 => {
                    return Ok(record)
                }
                state => return Err(state_error(state)),
            }
            match self.replace_retirement(record.clone()) {
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
                        .by_approval_id(&approval_id)
                        .map_err(map_store_error)?
                        .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    fn resume_retirement(
        &self,
        record: DeviceAuthorizationRecord,
        ports: &impl DeviceCertificateDeliveryPorts,
    ) -> Result<(), DeviceAuthorizationError> {
        let record = self.try_retirement(record, ports)?;
        self.completed_state_result(record)
    }

    fn completed_state_result(
        &self,
        record: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationError> {
        match record.state {
            DeviceAuthorizationState::DeliveryPending { .. }
            | DeviceAuthorizationState::Delivered { .. } => Ok(()),
            DeviceAuthorizationState::RetirementPending { last_failure, .. } => {
                Err(DeviceAuthorizationError::CertificateRetirementPending {
                    status: last_failure.map_or(
                        DeviceDeliveryRecoveryStatus::RevocationPending,
                        DeviceCertificateRetirementError::recovery_status,
                    ),
                })
            }
            DeviceAuthorizationState::DeliveryExpired { .. } => {
                Err(DeviceAuthorizationError::DeliveryExpired)
            }
            DeviceAuthorizationState::IssuanceFailed { failure, .. } => {
                Err(issuance_failure_error(failure))
            }
            DeviceAuthorizationState::Issuing { .. } => {
                Err(DeviceAuthorizationError::CertificateIssuanceInProgress)
            }
            _ => Err(state_error(&record.state)),
        }
    }

    /// Advances one due delivery or resumes one durable retirement by authorization ID.
    ///
    /// `database_now_unix_ms` must come from the durable store's database clock.
    /// It records when this sweep reserved retirement; the store must independently
    /// recheck the persisted deadline with database time in the reservation CAS.
    /// This entrypoint never contacts the CA while the row is `DeliveryPending`.
    pub(crate) fn retire_certificate_work_by_authorization_id(
        &self,
        authorization_id: &DeviceAuthorizationId,
        database_now_unix_ms: u64,
        retirement_port: &impl DeviceCertificateRetirementPort,
    ) -> Result<DeviceCertificateRetirementWorkOutcome, DeviceAuthorizationError> {
        const MAX_CAS_ATTEMPTS: usize = 5;
        let Some(mut record_value) = self
            .store
            .by_authorization_id(authorization_id)
            .map_err(map_store_error)?
        else {
            return Ok(DeviceCertificateRetirementWorkOutcome::NoWork);
        };

        for _ in 0..MAX_CAS_ATTEMPTS {
            match &record_value.state {
                DeviceAuthorizationState::RetirementPending { .. } => {
                    let retired = self.try_retirement(record_value, retirement_port)?;
                    return Ok(retirement_work_outcome(&retired));
                }
                DeviceAuthorizationState::DeliveryPending {
                    approval_id,
                    approver,
                    certificate,
                    delivery_id,
                    certificate_sha256,
                    delivery_deadline_unix_ms,
                    ..
                } => {
                    if database_now_unix_ms < *delivery_deadline_unix_ms
                        && database_now_unix_ms < certificate.not_after_unix_ms
                    {
                        return Ok(DeviceCertificateRetirementWorkOutcome::NoWork);
                    }
                    let expected_revision = record_value.revision;
                    let mut replacement = record_value.clone();
                    replacement.revision = expected_revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    replacement.state = DeviceAuthorizationState::RetirementPending {
                        approval_id: *approval_id,
                        approver: approver.clone(),
                        certificate: certificate.clone(),
                        certificate_sha256: *certificate_sha256,
                        delivery_id: Some(*delivery_id),
                        reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                        entered_at_unix_ms: database_now_unix_ms,
                        last_failure: None,
                    };
                    match self.store.compare_and_swap_due_delivery_to_retirement(
                        expected_revision,
                        replacement.clone(),
                    ) {
                        Ok(true) => {
                            return Ok(retirement_work_outcome(
                                &self.try_retirement(replacement, retirement_port)?,
                            ));
                        }
                        Ok(false) | Err(DeviceAuthorizationStoreError::Conflict) => {
                            record_value = self
                                .store
                                .by_authorization_id(authorization_id)
                                .map_err(map_store_error)?
                                .ok_or(DeviceAuthorizationError::ConcurrentTransition)?;
                        }
                        Err(error) => return Err(map_store_error(error)),
                    }
                }
                _ => return Ok(DeviceCertificateRetirementWorkOutcome::NoWork),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
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
        self.require_current_registration(&record)?;
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

    /// Polls with the device code. A ready certificate remains byte-identical
    /// and retryable until a matching acknowledgement commits before deadline.
    pub fn poll(
        &self,
        device_code: &str,
        now_unix_ms: u64,
        ports: &impl DeviceCertificateDeliveryPorts,
    ) -> Result<DeviceAuthorizationPoll, DeviceAuthorizationError> {
        let result = self.poll_with_snapshot(device_code, now_unix_ms, ports)?;
        match result.response {
            DeviceAuthorizationPoll::SlowDown { retry_after_ms, .. } => {
                Err(DeviceAuthorizationError::SlowDown { retry_after_ms })
            }
            response => Ok(response),
        }
    }

    /// Polls and returns the committed binding/state snapshot that produced
    /// the result. HTTP adapters must derive response metadata from this value.
    pub fn poll_with_snapshot(
        &self,
        device_code: &str,
        now_unix_ms: u64,
        ports: &impl DeviceCertificateDeliveryPorts,
    ) -> Result<DeviceAuthorizationPollResult, DeviceAuthorizationError> {
        let code_hash = device_code_hash(device_code)?;
        let snapshot = self
            .store
            .current_poll_snapshot(&code_hash, None, now_unix_ms)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        let mut record = snapshot.record;
        let database_now_unix_ms = snapshot.database_now_unix_ms;
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
        self.expire_if_due(&mut record, database_now_unix_ms)?;

        if let Some(last_poll_at_unix_ms) = record.last_poll_at_unix_ms {
            let next_allowed_at = last_poll_at_unix_ms.saturating_add(record.poll_interval_ms);
            if database_now_unix_ms < next_allowed_at {
                record.poll_interval_ms = record
                    .poll_interval_ms
                    .saturating_add(self.policy.slow_down_increment_ms)
                    .min(self.policy.maximum_poll_interval_ms);
                let next_allowed_at = last_poll_at_unix_ms.saturating_add(record.poll_interval_ms);
                let retry_after_ms = next_allowed_at.saturating_sub(database_now_unix_ms);
                let committed = self.replace_with_revision(record)?;
                let current = self.require_current_poll_snapshot(
                    &committed,
                    &code_hash,
                    database_now_unix_ms,
                )?;
                return Ok(DeviceAuthorizationPollResult {
                    response: DeviceAuthorizationPoll::SlowDown {
                        interval_ms: current.record.poll_interval_ms,
                        retry_after_ms,
                    },
                    snapshot: current,
                });
            }
        }

        record.last_poll_at_unix_ms = Some(database_now_unix_ms);
        match record.state.clone() {
            DeviceAuthorizationState::Pending
            | DeviceAuthorizationState::AwaitingWebAuthn { .. }
            | DeviceAuthorizationState::VerifyingWebAuthn { .. }
            | DeviceAuthorizationState::Issuing { .. } => {
                let committed = self.replace_with_revision(record)?;
                let current = self.require_current_poll_snapshot(
                    &committed,
                    &code_hash,
                    database_now_unix_ms,
                )?;
                Ok(DeviceAuthorizationPollResult {
                    response: DeviceAuthorizationPoll::Pending {
                        interval_ms: current.record.poll_interval_ms,
                    },
                    snapshot: current,
                })
            }
            DeviceAuthorizationState::DeliveryPending {
                approval_id,
                approver,
                decided_at_unix_ms: _,
                certificate,
                delivery_id,
                certificate_sha256,
                delivery_deadline_unix_ms,
            } => {
                if database_now_unix_ms >= delivery_deadline_unix_ms
                    || database_now_unix_ms >= certificate.not_after_unix_ms
                {
                    let retirement = DeviceAuthorizationState::RetirementPending {
                        approval_id,
                        approver,
                        certificate,
                        certificate_sha256,
                        delivery_id: Some(delivery_id),
                        reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                        entered_at_unix_ms: database_now_unix_ms,
                        last_failure: None,
                    };
                    let record = self.reserve_retirement(record, retirement)?;
                    let record = if matches!(
                        record.state,
                        DeviceAuthorizationState::RetirementPending { .. }
                    ) {
                        self.try_retirement(record, ports)?
                    } else {
                        record
                    };
                    let current = self.require_current_poll_snapshot(
                        &record,
                        &code_hash,
                        database_now_unix_ms,
                    )?;
                    Ok(DeviceAuthorizationPollResult {
                        response: poll_result_from_state(current.record.state.clone())?,
                        snapshot: current,
                    })
                } else {
                    let delivery = DeviceCertificateDelivery {
                        authorization_id: record.id,
                        delivery_id,
                        device_id: record.registration_binding.key().device_id.clone(),
                        authorization_generation: record
                            .registration_binding
                            .authorization_generation(),
                        certificate_der: certificate.certificate_der.clone(),
                        ca_chain_der: certificate.ca_chain_der.clone(),
                        scope: certificate.scope.clone(),
                        serial_number: certificate.serial_number.clone(),
                        not_after_unix_ms: certificate.not_after_unix_ms,
                        certificate_sha256,
                        csr_sha256: record.csr_sha256,
                        csr_spki_sha256: record.spki_sha256,
                        acknowledgement_deadline_unix_ms: delivery_deadline_unix_ms,
                    };
                    let committed = self.replace_with_revision(record)?;
                    let current = self.require_current_poll_snapshot(
                        &committed,
                        &code_hash,
                        database_now_unix_ms,
                    )?;
                    Ok(DeviceAuthorizationPollResult {
                        response: DeviceAuthorizationPoll::CertificateReady(delivery),
                        snapshot: current,
                    })
                }
            }
            DeviceAuthorizationState::RetirementPending { .. } => {
                let record = self.replace_and_reload(record)?;
                let record = self.try_retirement(record, ports)?;
                let current =
                    self.require_current_poll_snapshot(&record, &code_hash, database_now_unix_ms)?;
                Ok(DeviceAuthorizationPollResult {
                    response: poll_result_from_state(current.record.state.clone())?,
                    snapshot: current,
                })
            }
            DeviceAuthorizationState::Delivered { .. } => {
                let committed = self.replace_with_revision(record)?;
                let current = self.require_current_poll_snapshot(
                    &committed,
                    &code_hash,
                    database_now_unix_ms,
                )?;
                Ok(DeviceAuthorizationPollResult {
                    response: poll_result_from_state(current.record.state.clone())?,
                    snapshot: current,
                })
            }
            DeviceAuthorizationState::DeliveryExpired { .. } => {
                let current =
                    self.require_current_poll_snapshot(&record, &code_hash, database_now_unix_ms)?;
                Ok(DeviceAuthorizationPollResult {
                    response: poll_result_from_state(current.record.state.clone())?,
                    snapshot: current,
                })
            }
            DeviceAuthorizationState::IssuanceFailed { .. } => {
                let current =
                    self.require_current_poll_snapshot(&record, &code_hash, database_now_unix_ms)?;
                Ok(DeviceAuthorizationPollResult {
                    response: poll_result_from_state(current.record.state.clone())?,
                    snapshot: current,
                })
            }
            DeviceAuthorizationState::Denied { .. } => Err(DeviceAuthorizationError::Denied),
            DeviceAuthorizationState::Consumed { .. } => {
                Err(DeviceAuthorizationError::AlreadyConsumed)
            }
            DeviceAuthorizationState::Expired => Err(DeviceAuthorizationError::Expired),
        }
    }

    /// Commits a delivery receipt only while the certificate remains within
    /// its persisted acknowledgement window. Exact receipt replays are safe
    /// after the deadline because the original CAS proves timely commitment.
    pub fn acknowledge_delivery(
        &self,
        acknowledgement: &DeviceCertificateDeliveryAcknowledgement,
        ports: &impl DeviceCertificateDeliveryPorts,
    ) -> Result<DeviceCertificateDeliveryReceipt, DeviceAuthorizationError> {
        let mut record = self.lookup_by_device_code(&acknowledgement.device_code)?;
        if record.id != acknowledgement.authorization_id {
            return Err(DeviceAuthorizationError::InvalidDeliveryAcknowledgement);
        }

        for _ in 0..5 {
            match &record.state {
                DeviceAuthorizationState::Delivered { receipt, .. }
                    if receipt_matches_acknowledgement(receipt, acknowledgement) =>
                {
                    return Ok(receipt.clone());
                }
                DeviceAuthorizationState::Delivered { .. } => {
                    return Err(DeviceAuthorizationError::InvalidDeliveryAcknowledgement)
                }
                DeviceAuthorizationState::DeliveryPending {
                    approval_id,
                    approver,
                    certificate,
                    delivery_id,
                    certificate_sha256,
                    delivery_deadline_unix_ms,
                    ..
                } => {
                    if !delivery_matches_acknowledgement(
                        record.id,
                        record.csr_sha256,
                        record.spki_sha256,
                        *delivery_id,
                        *certificate_sha256,
                        acknowledgement,
                    ) {
                        return Err(DeviceAuthorizationError::InvalidDeliveryAcknowledgement);
                    }
                    let current_time = ports
                        .current_unix_ms()
                        .map_err(|_| DeviceAuthorizationError::ClockUnavailable)?;
                    if current_time >= *delivery_deadline_unix_ms
                        || current_time >= certificate.not_after_unix_ms
                    {
                        let retirement = DeviceAuthorizationState::RetirementPending {
                            approval_id: *approval_id,
                            approver: approver.clone(),
                            certificate: certificate.clone(),
                            certificate_sha256: *certificate_sha256,
                            delivery_id: Some(*delivery_id),
                            reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                            entered_at_unix_ms: current_time,
                            last_failure: None,
                        };
                        let record = self.reserve_retirement(record, retirement)?;
                        if let DeviceAuthorizationState::Delivered { receipt, .. } = &record.state {
                            return if receipt_matches_acknowledgement(receipt, acknowledgement) {
                                Ok(receipt.clone())
                            } else {
                                Err(DeviceAuthorizationError::InvalidDeliveryAcknowledgement)
                            };
                        }
                        let record = if matches!(
                            record.state,
                            DeviceAuthorizationState::RetirementPending { .. }
                        ) {
                            self.try_retirement(record, ports)?
                        } else {
                            record
                        };
                        return match record.state {
                            DeviceAuthorizationState::DeliveryExpired { .. } => {
                                Err(DeviceAuthorizationError::DeliveryExpired)
                            }
                            DeviceAuthorizationState::RetirementPending {
                                last_failure, ..
                            } => Err(DeviceAuthorizationError::CertificateRetirementPending {
                                status: last_failure.map_or(
                                    DeviceDeliveryRecoveryStatus::RevocationPending,
                                    DeviceCertificateRetirementError::recovery_status,
                                ),
                            }),
                            state => Err(state_error(&state)),
                        };
                    }

                    let receipt = DeviceCertificateDeliveryReceipt {
                        authorization_id: record.id,
                        delivery_id: *delivery_id,
                        device_id: record.registration_binding.key().device_id.clone(),
                        authorization_generation: record
                            .registration_binding
                            .authorization_generation(),
                        certificate_sha256: *certificate_sha256,
                        csr_sha256: record.csr_sha256,
                        csr_spki_sha256: record.spki_sha256,
                        acknowledged_at_unix_ms: current_time,
                    };
                    let mut replacement = record.clone();
                    replacement.state = DeviceAuthorizationState::Delivered {
                        approval_id: *approval_id,
                        receipt: receipt.clone(),
                    };
                    let expected_revision = replacement.revision;
                    replacement.revision = expected_revision
                        .checked_add(1)
                        .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
                    match self
                        .store
                        .compare_and_swap_registered_delivery_ack(expected_revision, replacement)
                    {
                        Ok(()) => return Ok(receipt),
                        Err(DeviceAuthorizationStoreError::Conflict) => {
                            record = self.lookup_by_device_code(&acknowledgement.device_code)?;
                        }
                        Err(error) => return Err(map_store_error(error)),
                    }
                }
                DeviceAuthorizationState::RetirementPending { reason, .. }
                    if *reason == DeviceCertificateRetirementReason::DeliveryDeadlineReached =>
                {
                    let record = self.try_retirement(record, ports)?;
                    return match record.state {
                        DeviceAuthorizationState::DeliveryExpired { .. } => {
                            Err(DeviceAuthorizationError::DeliveryExpired)
                        }
                        DeviceAuthorizationState::RetirementPending { last_failure, .. } => {
                            Err(DeviceAuthorizationError::CertificateRetirementPending {
                                status: last_failure.map_or(
                                    DeviceDeliveryRecoveryStatus::RevocationPending,
                                    DeviceCertificateRetirementError::recovery_status,
                                ),
                            })
                        }
                        state => Err(state_error(&state)),
                    };
                }
                _ => return Err(DeviceAuthorizationError::InvalidDeliveryAcknowledgement),
            }
        }
        Err(DeviceAuthorizationError::ConcurrentTransition)
    }

    fn lookup_by_device_code(
        &self,
        device_code: &str,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        let code_hash = device_code_hash(device_code)?;
        self.store
            .by_device_code_hash(&code_hash)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)
    }

    fn require_current_poll_snapshot(
        &self,
        record: &DeviceAuthorizationRecord,
        code_hash: &DeviceAuthorizationCodeHash,
        observed_at_unix_ms: u64,
    ) -> Result<DeviceAuthorizationPollSnapshot, DeviceAuthorizationError> {
        let current = self
            .store
            .current_poll_snapshot(code_hash, Some(record.revision), observed_at_unix_ms)
            .map_err(map_store_error)?
            .ok_or(DeviceAuthorizationError::InvalidCode)?;
        if current.record.id != record.id
            || current.record.revision != record.revision
            || current.record.device_code_hash != *code_hash
            || current.record.device_code_generation != record.device_code_generation
            || current.record.registration_binding != record.registration_binding
        {
            return Err(DeviceAuthorizationError::InvalidCode);
        }
        Ok(current)
    }

    fn replace_and_reload(
        &self,
        mut record: DeviceAuthorizationRecord,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        let revision = record.revision;
        self.replace(record.clone())?;
        record.revision = revision
            .checked_add(1)
            .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
        Ok(record)
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

    fn require_current_registration(
        &self,
        record: &DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationError> {
        self.store
            .require_current_registered_record(record)
            .map_err(map_store_error)
    }

    fn expire_if_due(
        &self,
        record: &mut DeviceAuthorizationRecord,
        now_unix_ms: u64,
    ) -> Result<(), DeviceAuthorizationError> {
        // `Issuing` may already have committed at the CA, and delivery has its
        // own deadline and retirement protocol. Only pre-issuance states use
        // the user authorization TTL.
        if matches!(
            record.state,
            DeviceAuthorizationState::Issuing { .. }
                | DeviceAuthorizationState::DeliveryPending { .. }
                | DeviceAuthorizationState::Delivered { .. }
                | DeviceAuthorizationState::RetirementPending { .. }
                | DeviceAuthorizationState::DeliveryExpired { .. }
                | DeviceAuthorizationState::IssuanceFailed { .. }
        ) {
            return Ok(());
        }
        if now_unix_ms < record.expires_at_unix_ms {
            return Ok(());
        }
        match &record.state {
            DeviceAuthorizationState::Pending
            | DeviceAuthorizationState::AwaitingWebAuthn { .. }
            | DeviceAuthorizationState::VerifyingWebAuthn { .. } => {
                record.state = DeviceAuthorizationState::Expired;
                self.replace(record.clone())?;
                *record = self
                    .store
                    .by_device_code_hash(&record.device_code_hash)
                    .map_err(map_store_error)?
                    .ok_or(DeviceAuthorizationError::InvalidCode)?;
                Err(DeviceAuthorizationError::Expired)
            }
            DeviceAuthorizationState::Issuing { .. }
            | DeviceAuthorizationState::DeliveryPending { .. }
            | DeviceAuthorizationState::Delivered { .. }
            | DeviceAuthorizationState::RetirementPending { .. }
            | DeviceAuthorizationState::DeliveryExpired { .. }
            | DeviceAuthorizationState::IssuanceFailed { .. } => Ok(()),
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
        replacement: DeviceAuthorizationRecord,
    ) -> Result<(), DeviceAuthorizationError> {
        self.replace_with_revision(replacement).map(|_| ())
    }

    fn replace_with_revision(
        &self,
        mut replacement: DeviceAuthorizationRecord,
    ) -> Result<DeviceAuthorizationRecord, DeviceAuthorizationError> {
        let expected_revision = replacement.revision;
        replacement.revision = expected_revision
            .checked_add(1)
            .ok_or(DeviceAuthorizationError::RevisionExhausted)?;
        self.store
            .compare_and_swap_registered(expected_revision, replacement.clone())
            .map_err(map_store_error)?;
        Ok(replacement)
    }

    fn replace_retirement(
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

fn registration_binding_matches(
    binding: &DeviceAuthorizationRegistrationBinding,
    scope: &DeviceAuthorizationScope,
    csr_sha256: &[u8; 32],
    spki_sha256: &[u8; 32],
) -> bool {
    let key = binding.key();
    !binding.binding_id().iter().all(|byte| *byte == 0)
        && binding.authorization_generation() > 0
        && key.organization_id == scope.organization_id
        && key.workspace_id == scope.workspace_id
        && !key.device_id.trim().is_empty()
        && key.device_id.len() <= 256
        && key.device_id == key.device_id.trim()
        && !key.device_id.chars().any(char::is_control)
        && binding.csr_sha256() == csr_sha256
        && binding.spki_sha256() == spki_sha256
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

fn device_code_hash(
    device_code: &str,
) -> Result<DeviceAuthorizationCodeHash, DeviceAuthorizationError> {
    if device_code.len() != 64 || !device_code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DeviceAuthorizationError::InvalidCode);
    }
    Ok(hash_code(
        b"device",
        device_code.to_ascii_lowercase().as_bytes(),
    ))
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
        | DeviceAuthorizationState::DeliveryPending { .. }
        | DeviceAuthorizationState::Delivered { .. }
        | DeviceAuthorizationState::RetirementPending { .. } => {
            DeviceAuthorizationError::AlreadyFinal
        }
        DeviceAuthorizationState::DeliveryExpired { .. } => {
            DeviceAuthorizationError::DeliveryExpired
        }
        DeviceAuthorizationState::IssuanceFailed { failure, .. } => {
            issuance_failure_error(*failure)
        }
        DeviceAuthorizationState::Denied { .. } => DeviceAuthorizationError::Denied,
        DeviceAuthorizationState::Consumed { .. } => DeviceAuthorizationError::AlreadyConsumed,
        DeviceAuthorizationState::Expired => DeviceAuthorizationError::Expired,
    }
}

fn issuance_failure_error(failure: DeviceCertificateIssuanceFailure) -> DeviceAuthorizationError {
    match failure {
        DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit
        | DeviceCertificateIssuanceFailure::CertificateExpiredBeforeDeliveryRetired => {
            DeviceAuthorizationError::CertificateSigningFailed
        }
        DeviceCertificateIssuanceFailure::MisboundCertificateRetired => {
            DeviceAuthorizationError::CertificateBindingMismatch
        }
    }
}

fn state_approval_id(state: &DeviceAuthorizationState) -> Option<DeviceAuthorizationId> {
    match state {
        DeviceAuthorizationState::AwaitingWebAuthn { approval_id, .. }
        | DeviceAuthorizationState::VerifyingWebAuthn { approval_id, .. }
        | DeviceAuthorizationState::Issuing { approval_id, .. }
        | DeviceAuthorizationState::DeliveryPending { approval_id, .. }
        | DeviceAuthorizationState::Delivered { approval_id, .. }
        | DeviceAuthorizationState::RetirementPending { approval_id, .. }
        | DeviceAuthorizationState::DeliveryExpired { approval_id, .. }
        | DeviceAuthorizationState::IssuanceFailed { approval_id, .. } => Some(*approval_id),
        DeviceAuthorizationState::Pending
        | DeviceAuthorizationState::Denied { .. }
        | DeviceAuthorizationState::Consumed { .. }
        | DeviceAuthorizationState::Expired => None,
    }
}

fn retirement_entered_at(state: &DeviceAuthorizationState) -> u64 {
    match state {
        DeviceAuthorizationState::RetirementPending {
            entered_at_unix_ms, ..
        } => *entered_at_unix_ms,
        _ => 0,
    }
}

fn retirement_work_outcome(
    record: &DeviceAuthorizationRecord,
) -> DeviceCertificateRetirementWorkOutcome {
    match &record.state {
        DeviceAuthorizationState::RetirementPending { last_failure, .. } => {
            DeviceCertificateRetirementWorkOutcome::Pending(last_failure.map_or(
                DeviceDeliveryRecoveryStatus::RevocationPending,
                DeviceCertificateRetirementError::recovery_status,
            ))
        }
        DeviceAuthorizationState::DeliveryExpired { .. }
        | DeviceAuthorizationState::IssuanceFailed {
            certificate_sha256: Some(_),
            ..
        } => DeviceCertificateRetirementWorkOutcome::Retired,
        _ => DeviceCertificateRetirementWorkOutcome::NoWork,
    }
}

fn retirement_transition_allowed(
    current: &DeviceAuthorizationState,
    target: &DeviceAuthorizationState,
) -> bool {
    match (current, target) {
        (
            DeviceAuthorizationState::Issuing {
                approval_id: current_id,
                approver: current_approver,
                ..
            },
            DeviceAuthorizationState::RetirementPending {
                approval_id: target_id,
                approver: target_approver,
                delivery_id: None,
                reason:
                    DeviceCertificateRetirementReason::MisboundCertificate
                    | DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery,
                ..
            },
        ) => current_id == target_id && current_approver == target_approver,
        (
            DeviceAuthorizationState::DeliveryPending {
                approval_id: current_id,
                approver: current_approver,
                certificate: current_certificate,
                delivery_id: current_delivery_id,
                certificate_sha256: current_certificate_sha256,
                ..
            },
            DeviceAuthorizationState::RetirementPending {
                approval_id: target_id,
                approver: target_approver,
                certificate: target_certificate,
                certificate_sha256: target_certificate_sha256,
                delivery_id: Some(target_delivery_id),
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                ..
            },
        ) => {
            current_id == target_id
                && current_approver == target_approver
                && current_certificate == target_certificate
                && current_certificate_sha256 == target_certificate_sha256
                && current_delivery_id == target_delivery_id
        }
        _ => false,
    }
}

fn retirement_state_already_resolved(
    current: &DeviceAuthorizationState,
    target: &DeviceAuthorizationState,
) -> bool {
    let DeviceAuthorizationState::RetirementPending {
        approval_id: target_id,
        certificate,
        certificate_sha256: target_hash,
        delivery_id: target_delivery_id,
        reason: target_reason,
        ..
    } = target
    else {
        return false;
    };
    match current {
        DeviceAuthorizationState::RetirementPending {
            approval_id: current_id,
            certificate: current_certificate,
            certificate_sha256: current_hash,
            delivery_id: current_delivery_id,
            reason: current_reason,
            ..
        } => {
            current_id == target_id
                && current_certificate == certificate
                && current_hash == target_hash
                && current_delivery_id == target_delivery_id
                && current_reason == target_reason
        }
        DeviceAuthorizationState::DeliveryExpired {
            approval_id: current_id,
            delivery_id: current_delivery_id,
            certificate_sha256: current_hash,
            ..
        } => {
            *target_reason == DeviceCertificateRetirementReason::DeliveryDeadlineReached
                && current_id == target_id
                && Some(current_delivery_id) == target_delivery_id.as_ref()
                && current_hash == target_hash
        }
        DeviceAuthorizationState::IssuanceFailed {
            approval_id: current_id,
            failure: current_failure,
            certificate_sha256: Some(current_hash),
            ..
        } => {
            current_id == target_id
                && current_hash == target_hash
                && matches!(
                    (*target_reason, *current_failure),
                    (
                        DeviceCertificateRetirementReason::MisboundCertificate,
                        DeviceCertificateIssuanceFailure::MisboundCertificateRetired
                    ) | (
                        DeviceCertificateRetirementReason::CertificateExpiredBeforeDelivery,
                        DeviceCertificateIssuanceFailure::CertificateExpiredBeforeDeliveryRetired
                    )
                )
        }
        DeviceAuthorizationState::Delivered {
            approval_id: current_id,
            receipt,
        } => {
            *target_reason == DeviceCertificateRetirementReason::DeliveryDeadlineReached
                && current_id == target_id
                && Some(&receipt.delivery_id) == target_delivery_id.as_ref()
                && &receipt.certificate_sha256 == target_hash
        }
        _ => false,
    }
}

fn receipt_matches_acknowledgement(
    receipt: &DeviceCertificateDeliveryReceipt,
    acknowledgement: &DeviceCertificateDeliveryAcknowledgement,
) -> bool {
    receipt.authorization_id == acknowledgement.authorization_id
        && receipt.delivery_id == acknowledgement.delivery_id
        && receipt.certificate_sha256 == acknowledgement.certificate_sha256
        && receipt.csr_sha256 == acknowledgement.csr_sha256
        && receipt.csr_spki_sha256 == acknowledgement.csr_spki_sha256
}

fn delivery_matches_acknowledgement(
    authorization_id: DeviceAuthorizationId,
    csr_sha256: [u8; 32],
    csr_spki_sha256: [u8; 32],
    delivery_id: DeviceAuthorizationId,
    certificate_sha256: [u8; 32],
    acknowledgement: &DeviceCertificateDeliveryAcknowledgement,
) -> bool {
    acknowledgement.authorization_id == authorization_id
        && acknowledgement.delivery_id == delivery_id
        && acknowledgement.certificate_sha256 == certificate_sha256
        && acknowledgement.csr_sha256 == csr_sha256
        && acknowledgement.csr_spki_sha256 == csr_spki_sha256
}

fn poll_result_from_state(
    state: DeviceAuthorizationState,
) -> Result<DeviceAuthorizationPoll, DeviceAuthorizationError> {
    match state {
        DeviceAuthorizationState::Delivered { receipt, .. } => {
            Ok(DeviceAuthorizationPoll::Delivered(receipt))
        }
        DeviceAuthorizationState::RetirementPending { last_failure, .. } => {
            Ok(DeviceAuthorizationPoll::RecoveryRequired {
                status: last_failure.map_or(
                    DeviceDeliveryRecoveryStatus::RevocationPending,
                    DeviceCertificateRetirementError::recovery_status,
                ),
            })
        }
        DeviceAuthorizationState::DeliveryExpired {
            delivery_id,
            certificate_sha256,
            ..
        } => Ok(DeviceAuthorizationPoll::DeliveryExpired {
            delivery_id,
            certificate_sha256,
        }),
        DeviceAuthorizationState::IssuanceFailed { failure, .. } => {
            Ok(DeviceAuthorizationPoll::IssuanceFailed { failure })
        }
        other => Err(state_error(&other)),
    }
}

fn map_store_error(error: DeviceAuthorizationStoreError) -> DeviceAuthorizationError {
    match error {
        DeviceAuthorizationStoreError::CodeCollision => DeviceAuthorizationError::CodeCollision,
        DeviceAuthorizationStoreError::Conflict => DeviceAuthorizationError::ConcurrentTransition,
        DeviceAuthorizationStoreError::RecoveryLimit => DeviceAuthorizationError::TooManyAttempts,
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
            registration_binding: &DeviceAuthorizationRegistrationBinding,
            _csr_der: &[u8],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError> {
            Ok(test_certificate_for(
                registration_binding,
                b"test-only-not-a-real-certificate",
                &[1],
                issued_at_unix_ms.saturating_add(60_000),
            ))
        }
    }

    impl DeviceCertificateRetirementPort for TestApprovalPorts {
        fn retire_or_confirm(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            _certificate_sha256: &[u8; 32],
            _certificate: &IssuedDeviceCertificate,
            _reason: DeviceCertificateRetirementReason,
        ) -> Result<(), DeviceCertificateRetirementError> {
            Ok(())
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
        state.extend_from_slice(&context.registration_binding_id);
        for value in [
            context.scope.organization_id.as_bytes(),
            context.scope.workspace_id.as_bytes(),
            context.device_key.device_id.as_bytes(),
        ] {
            state.extend_from_slice(&(value.len() as u64).to_be_bytes());
            state.extend_from_slice(value);
        }
        state.extend_from_slice(&context.authorization_generation.to_be_bytes());
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
        Allow,
        SignerReturnsExpiredCertificate,
    }

    #[derive(Default)]
    struct CoordinatedApprovalState {
        verifier_released: bool,
        verifier_calls: usize,
        credential_counter_updates: usize,
        consumed_assertions: BTreeMap<DeviceAuthorizationId, [u8; 32]>,
        issuer_calls: usize,
        retirement_calls: usize,
        retirement_failures_remaining: usize,
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

        fn set_retirement_failures_remaining(&self, count: usize) {
            self.state
                .lock()
                .expect("verifier state")
                .retirement_failures_remaining = count;
        }

        fn retirement_calls(&self) -> usize {
            self.state.lock().expect("verifier state").retirement_calls
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
            registration_binding: &DeviceAuthorizationRegistrationBinding,
            _csr_der: &[u8],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError> {
            self.state.lock().expect("verifier state").issuer_calls += 1;
            let not_after_unix_ms =
                if self.behavior == VerifierBehavior::SignerReturnsExpiredCertificate {
                    issued_at_unix_ms.saturating_add(1_000)
                } else {
                    issued_at_unix_ms.saturating_add(60_000)
                };
            if self.behavior == VerifierBehavior::SignerReturnsExpiredCertificate {
                self.current_time_unix_ms
                    .store(not_after_unix_ms, Ordering::SeqCst);
            }
            Ok(test_certificate_for(
                registration_binding,
                b"test-only-coordinated-certificate",
                &[3],
                not_after_unix_ms,
            ))
        }
    }

    impl DeviceCertificateRetirementPort for CoordinatedApprovalPorts {
        fn retire_or_confirm(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            _certificate_sha256: &[u8; 32],
            _certificate: &IssuedDeviceCertificate,
            _reason: DeviceCertificateRetirementReason,
        ) -> Result<(), DeviceCertificateRetirementError> {
            let mut state = self.state.lock().expect("verifier state");
            state.retirement_calls += 1;
            if state.retirement_failures_remaining > 0 {
                state.retirement_failures_remaining -= 1;
                Err(DeviceCertificateRetirementError::OutcomeUnknown)
            } else {
                Ok(())
            }
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
            registration_binding: &DeviceAuthorizationRegistrationBinding,
            _csr_der: &[u8],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError> {
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
            let certificate = test_certificate_for(
                registration_binding,
                b"test-only-idempotent-certificate",
                &[1],
                issued_at_unix_ms.saturating_add(60_000),
            );
            state.certificate = Some(certificate.clone());
            state.in_flight = false;
            self.changed.notify_all();
            Ok(certificate)
        }
    }

    impl DeviceCertificateRetirementPort for BlockingApprovalPorts {
        fn retire_or_confirm(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            _certificate_sha256: &[u8; 32],
            _certificate: &IssuedDeviceCertificate,
            _reason: DeviceCertificateRetirementReason,
        ) -> Result<(), DeviceCertificateRetirementError> {
            Ok(())
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
        definitive_no_commit: bool,
        misbound_certificate: bool,
        misbound_device_identity: bool,
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
            registration_binding: &DeviceAuthorizationRegistrationBinding,
            _csr_der: &[u8],
            issued_at_unix_ms: u64,
        ) -> Result<IssuedDeviceCertificate, DeviceCertificateIssuanceError> {
            let mut calls = self.calls.lock().expect("signer call count");
            *calls += 1;
            let mut state = self.state.lock().expect("signer state");
            if let Some((existing_id, certificate)) = &*state {
                return if existing_id == enrollment_id {
                    Ok(certificate.clone())
                } else {
                    Err(DeviceCertificateIssuanceError::DefinitiveNoCommit)
                };
            }
            if self.definitive_no_commit {
                return Err(DeviceCertificateIssuanceError::DefinitiveNoCommit);
            }
            let mut certificate = test_certificate_for(
                registration_binding,
                b"test-only-committed-before-timeout",
                &[2],
                issued_at_unix_ms.saturating_add(60_000),
            );
            if self.misbound_certificate {
                certificate.scope.workspace_id = "wrong-workspace".to_string();
            }
            if self.misbound_device_identity {
                certificate.device_key.device_id = "wrong-device".to_string();
            }
            *state = Some((*enrollment_id, certificate));
            if self.misbound_certificate || self.misbound_device_identity {
                return Ok(state.as_ref().expect("certificate stored").1.clone());
            }
            // Model a lost response after the CA committed the certificate.
            Err(DeviceCertificateIssuanceError::OutcomeUnknown)
        }
    }

    impl DeviceCertificateRetirementPort for UncertainOnceApprovalPorts {
        fn retire_or_confirm(
            &self,
            _authorization_id: &DeviceAuthorizationId,
            _certificate_sha256: &[u8; 32],
            _certificate: &IssuedDeviceCertificate,
            _reason: DeviceCertificateRetirementReason,
        ) -> Result<(), DeviceCertificateRetirementError> {
            Ok(())
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
            maximum_recovery_attempts: 3,
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

    #[derive(Clone, Default)]
    struct LegacyDeviceAuthorizationStore(InMemoryDeviceAuthorizationStore);

    impl DeviceAuthorizationStore for LegacyDeviceAuthorizationStore {
        fn insert(
            &self,
            record: DeviceAuthorizationRecord,
        ) -> Result<(), DeviceAuthorizationStoreError> {
            self.0.insert(record)
        }

        fn by_device_code_hash(
            &self,
            code_hash: &DeviceAuthorizationCodeHash,
        ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
            self.0.by_device_code_hash(code_hash)
        }

        fn by_user_code_candidates(
            &self,
            candidates: &[VersionedUserCodeDigest],
        ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
            self.0.by_user_code_candidates(candidates)
        }

        fn user_code_key_versions(&self) -> Result<Vec<u32>, DeviceAuthorizationStoreError> {
            self.0.user_code_key_versions()
        }

        fn by_approval_id(
            &self,
            approval_id: &DeviceAuthorizationId,
        ) -> Result<Option<DeviceAuthorizationRecord>, DeviceAuthorizationStoreError> {
            self.0.by_approval_id(approval_id)
        }

        fn compare_and_swap(
            &self,
            expected_revision: u64,
            replacement: DeviceAuthorizationRecord,
        ) -> Result<(), DeviceAuthorizationStoreError> {
            self.0.compare_and_swap(expected_revision, replacement)
        }

        fn compare_and_swap_delivery_ack(
            &self,
            expected_revision: u64,
            replacement: DeviceAuthorizationRecord,
        ) -> Result<(), DeviceAuthorizationStoreError> {
            self.0
                .compare_and_swap_delivery_ack(expected_revision, replacement)
        }
    }

    fn start<S, L>(manager: &DeviceAuthorizationManager<S, L>) -> DeviceAuthorizationStart
    where
        S: DeviceAuthorizationStore,
        L: UserCodeAttemptLimiter,
    {
        static NEXT_TEST_GENERATION: AtomicU64 = AtomicU64::new(1);
        let generation = NEXT_TEST_GENERATION.fetch_add(1, Ordering::SeqCst);
        start_with_generation(manager, generation)
    }

    fn start_with_generation<S, L>(
        manager: &DeviceAuthorizationManager<S, L>,
        generation: u64,
    ) -> DeviceAuthorizationStart
    where
        S: DeviceAuthorizationStore,
        L: UserCodeAttemptLimiter,
    {
        manager
            .begin(
                test_authorization_request(&scope(), generation),
                100,
                &TestCsrValidator,
            )
            .expect("begin enrollment")
    }

    fn test_authorization_request(
        request_scope: &DeviceAuthorizationScope,
        generation: u64,
    ) -> DeviceAuthorizationRequest {
        let csr_der = b"test-only-csr".to_vec();
        DeviceAuthorizationRequest {
            scope: request_scope.clone(),
            csr_der: csr_der.clone(),
            registration_key_digest: DeviceRegistrationKeyDigest::from_secret(
                &[u8::try_from(generation % 255 + 1).expect("bounded generation"); 32],
            ),
            registration_binding: test_registration_binding(request_scope, &csr_der, generation),
        }
    }

    fn test_registration_binding(
        scope: &DeviceAuthorizationScope,
        csr_der: &[u8],
        generation: u64,
    ) -> DeviceAuthorizationRegistrationBinding {
        let authority_result = TestDirectoryRegistrationBinding {
            binding_id: [u8::try_from(generation % 255 + 1).expect("bounded generation"); 16],
            key: DeviceAuthorizationDeviceKey {
                organization_id: scope.organization_id.clone(),
                workspace_id: scope.workspace_id.clone(),
                device_id: "test-stable-device-id".to_string(),
            },
            authorization_generation: generation,
            csr_sha256: sha256(csr_der),
            spki_sha256: [7; 32],
        };
        DeviceAuthorizationRegistrationBinding::from_verified_directory_binding(&authority_result)
    }

    struct TestDirectoryRegistrationBinding {
        binding_id: [u8; 16],
        key: DeviceAuthorizationDeviceKey,
        authorization_generation: u64,
        csr_sha256: [u8; 32],
        spki_sha256: [u8; 32],
    }

    impl VerifiedDirectoryRegistrationBinding for TestDirectoryRegistrationBinding {
        fn binding_id(&self) -> &[u8; 16] {
            &self.binding_id
        }

        fn organization_id(&self) -> &str {
            &self.key.organization_id
        }

        fn workspace_id(&self) -> &str {
            &self.key.workspace_id
        }

        fn device_id(&self) -> &str {
            &self.key.device_id
        }

        fn authorization_generation(&self) -> u64 {
            self.authorization_generation
        }

        fn csr_sha256(&self) -> &[u8; 32] {
            &self.csr_sha256
        }

        fn spki_sha256(&self) -> &[u8; 32] {
            &self.spki_sha256
        }
    }

    fn test_certificate_for(
        registration_binding: &DeviceAuthorizationRegistrationBinding,
        certificate_der: &[u8],
        serial_number: &[u8],
        not_after_unix_ms: u64,
    ) -> IssuedDeviceCertificate {
        let key = registration_binding.key().clone();
        IssuedDeviceCertificate {
            certificate_der: certificate_der.to_vec(),
            ca_chain_der: vec![],
            serial_number: serial_number.to_vec(),
            registration_binding_id: *registration_binding.binding_id(),
            device_key: key.clone(),
            authorization_generation: registration_binding.authorization_generation(),
            scope: DeviceAuthorizationScope {
                organization_id: key.organization_id,
                workspace_id: key.workspace_id,
            },
            spki_sha256: *registration_binding.spki_sha256(),
            not_after_unix_ms,
        }
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
            registration_binding_id: *record.registration_binding.binding_id(),
            device_key: record.registration_binding.key().clone(),
            authorization_generation: record.registration_binding.authorization_generation(),
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
    fn approval_is_scope_bound_and_delivery_retries_until_acknowledged() {
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

        let delivery = match manager
            .poll(&start.device_code, 1_100, &TestApprovalPorts)
            .expect("first poll")
        {
            DeviceAuthorizationPoll::CertificateReady(delivery) => delivery,
            result => panic!("unexpected poll result: {result:?}"),
        };
        assert_eq!(delivery.scope, scope());
        assert_eq!(delivery.device_id, start.device_id);
        assert_eq!(
            delivery.authorization_generation,
            start.authorization_generation
        );
        let retry = manager
            .poll(&start.device_code, 2_200, &TestApprovalPorts)
            .expect("retry poll returns same certificate");
        assert_eq!(
            retry,
            DeviceAuthorizationPoll::CertificateReady(delivery.clone())
        );
        assert_eq!(
            manager.begin(
                test_authorization_request(
                    &scope(),
                    start.authorization_generation.saturating_add(1),
                ),
                100,
                &TestCsrValidator,
            ),
            Err(DeviceAuthorizationError::ConcurrentTransition)
        );
        let acknowledgement = DeviceCertificateDeliveryAcknowledgement {
            authorization_id: delivery.authorization_id,
            device_code: start.device_code.clone(),
            delivery_id: delivery.delivery_id,
            certificate_sha256: delivery.certificate_sha256,
            csr_sha256: delivery.csr_sha256,
            csr_spki_sha256: delivery.csr_spki_sha256,
        };
        let receipt = manager
            .acknowledge_delivery(&acknowledgement, &TestApprovalPorts)
            .expect("delivery ACK");
        assert_eq!(receipt.device_id, start.device_id);
        assert_eq!(
            receipt.authorization_generation,
            start.authorization_generation
        );
        let rotated_start =
            start_with_generation(&manager, start.authorization_generation.saturating_add(100));
        assert_eq!(rotated_start.device_id, start.device_id);
        assert!(rotated_start.authorization_generation > start.authorization_generation);
        assert_eq!(
            manager.acknowledge_delivery(&acknowledgement, &TestApprovalPorts),
            Ok(receipt.clone())
        );
        assert_eq!(
            manager.poll(&start.device_code, 3_300, &TestApprovalPorts),
            Err(DeviceAuthorizationError::ConcurrentTransition)
        );
    }

    #[test]
    fn stale_generation_cannot_commit_new_ack() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[29; 32],
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
            .expect("issue fake test certificate");
        let delivery = match manager
            .poll(&start.device_code, 1_100, &TestApprovalPorts)
            .expect("test delivery")
        {
            DeviceAuthorizationPoll::CertificateReady(delivery) => delivery,
            result => panic!("unexpected poll result: {result:?}"),
        };

        // Bypass the registered test operation to model a Directory generation
        // change racing a pending ACK; production adapters must make this
        // impossible by sharing the transaction and row lock.
        let old_record = store
            .by_device_code_hash(&hash_code(
                b"device",
                start.device_code.to_ascii_lowercase().as_bytes(),
            ))
            .expect("load pending authorization")
            .expect("authorization record");
        store
            .insert(DeviceAuthorizationRecord {
                id: [0x99; 16],
                registration_binding: test_registration_binding(
                    &old_record.scope,
                    &old_record.csr_der,
                    start.authorization_generation.saturating_add(1),
                ),
                registration_key_digest: Some(DeviceRegistrationKeyDigest::from_secret(
                    &[0x99; 32],
                )),
                device_code_generation: 1,
                device_code_hash: [0xaa; 32],
                user_code_digest: manager
                    .user_code_keys
                    .digest_for_storage("ABCDEFGHJKLM")
                    .expect("hash new test code"),
                scope: old_record.scope.clone(),
                csr_der: old_record.csr_der.clone(),
                csr_sha256: old_record.csr_sha256,
                spki_sha256: old_record.spki_sha256,
                created_at_unix_ms: old_record.created_at_unix_ms,
                expires_at_unix_ms: old_record.expires_at_unix_ms,
                poll_interval_ms: old_record.poll_interval_ms,
                last_poll_at_unix_ms: None,
                revision: 0,
                state: DeviceAuthorizationState::Pending,
            })
            .expect("simulate Directory generation transition outside composite store");

        assert_eq!(
            manager.acknowledge_delivery(
                &DeviceCertificateDeliveryAcknowledgement {
                    authorization_id: delivery.authorization_id,
                    device_code: start.device_code,
                    delivery_id: delivery.delivery_id,
                    certificate_sha256: delivery.certificate_sha256,
                    csr_sha256: delivery.csr_sha256,
                    csr_spki_sha256: delivery.csr_spki_sha256,
                },
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::ConcurrentTransition)
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
            manager.begin(
                test_authorization_request(
                    &scope(),
                    start.authorization_generation.saturating_add(1),
                ),
                100,
                &TestCsrValidator,
            ),
            Err(DeviceAuthorizationError::ConcurrentTransition)
        );
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
            DeviceAuthorizationState::DeliveryPending { .. }
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
            DeviceAuthorizationState::DeliveryPending { .. }
        ));
        assert_eq!(*ports.calls.lock().expect("signer call count"), 2);
    }

    #[test]
    fn definitive_signer_rejection_is_persisted_as_terminal_failure() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[22; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let ports = UncertainOnceApprovalPorts {
            definitive_no_commit: true,
            ..UncertainOnceApprovalPorts::default()
        };

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::CertificateSigningFailed)
        );
        let record = store
            .by_approval_id(&challenge.approval_id)
            .expect("read failed issuance")
            .expect("approval record");
        assert!(matches!(
            record.state,
            DeviceAuthorizationState::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit,
                ..
            }
        ));
        assert_eq!(
            manager.poll(&start.device_code, 1_100, &TestApprovalPorts),
            Ok(DeviceAuthorizationPoll::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::SignerRejectedWithoutCommit,
            })
        );
    }

    #[test]
    fn certificate_expiring_during_signer_call_is_retired_before_failure() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let (ports, _verifier_started) =
            CoordinatedApprovalPorts::new(VerifierBehavior::SignerReturnsExpiredCertificate, 300);
        let challenge = manager
            .begin_approval(&start.user_code, &scope(), &user(), &[25; 32], 200, &ports)
            .expect("challenge");

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::CertificateSigningFailed)
        );
        let record = store
            .by_approval_id(&challenge.approval_id)
            .expect("read expired signer result")
            .expect("approval record");
        assert!(matches!(
            record.state,
            DeviceAuthorizationState::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::CertificateExpiredBeforeDeliveryRetired,
                ..
            }
        ));
        assert_eq!(ports.issuer_calls(), 1);
        assert_eq!(ports.retirement_calls(), 1);
        assert_eq!(
            manager.poll(&start.device_code, 1_300, &ports),
            Ok(DeviceAuthorizationPoll::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::CertificateExpiredBeforeDeliveryRetired,
            })
        );
    }

    #[test]
    fn misbound_certificate_is_retired_before_failure_becomes_terminal() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[23; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let ports = UncertainOnceApprovalPorts {
            misbound_certificate: true,
            ..UncertainOnceApprovalPorts::default()
        };

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::CertificateBindingMismatch)
        );
        let record = store
            .by_approval_id(&challenge.approval_id)
            .expect("read retired misbound certificate")
            .expect("approval record");
        assert!(matches!(
            record.state,
            DeviceAuthorizationState::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::MisboundCertificateRetired,
                ..
            }
        ));
        assert_eq!(
            manager.poll(&start.device_code, 1_100, &TestApprovalPorts),
            Ok(DeviceAuthorizationPoll::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::MisboundCertificateRetired,
            })
        );
    }

    #[test]
    fn issuer_device_identity_attestation_must_match_directory_binding() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[27; 32],
                200,
                &TestApprovalPorts,
            )
            .expect("challenge");
        let ports = UncertainOnceApprovalPorts {
            misbound_device_identity: true,
            ..UncertainOnceApprovalPorts::default()
        };

        assert_eq!(
            manager.complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            ),
            Err(DeviceAuthorizationError::CertificateBindingMismatch)
        );
        assert!(matches!(
            store
                .by_approval_id(&challenge.approval_id)
                .expect("read quarantined issuer result")
                .expect("approval record")
                .state,
            DeviceAuthorizationState::IssuanceFailed {
                failure: DeviceCertificateIssuanceFailure::MisboundCertificateRetired,
                ..
            }
        ));
    }

    #[test]
    fn delivery_expiry_stays_recovery_pending_until_retirement_is_confirmed() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let (ports, _verifier_started) =
            CoordinatedApprovalPorts::new(VerifierBehavior::Allow, 300);
        let challenge = manager
            .begin_approval(&start.user_code, &scope(), &user(), &[24; 32], 200, &ports)
            .expect("challenge");
        manager
            .complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            )
            .expect("approval creates delivery");
        let delivery = match manager
            .poll(&start.device_code, 1_100, &ports)
            .expect("initial certificate poll")
        {
            DeviceAuthorizationPoll::CertificateReady(delivery) => delivery,
            result => panic!("unexpected poll result: {result:?}"),
        };
        ports.set_retirement_failures_remaining(2);
        ports.set_current_time(60_300);

        assert_eq!(
            manager.poll(&start.device_code, 60_300, &ports),
            Ok(DeviceAuthorizationPoll::RecoveryRequired {
                status: DeviceDeliveryRecoveryStatus::RevocationPending,
            })
        );
        let pending = store
            .by_approval_id(&challenge.approval_id)
            .expect("read retirement pending")
            .expect("approval record");
        let (delivery_id, certificate_sha256) = match &pending.state {
            DeviceAuthorizationState::RetirementPending {
                delivery_id: Some(delivery_id),
                certificate_sha256,
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                last_failure: Some(DeviceCertificateRetirementError::OutcomeUnknown),
                ..
            } => (*delivery_id, *certificate_sha256),
            _ => panic!("delivery retirement must remain recoverable"),
        };
        assert!(matches!(
            pending.state,
            DeviceAuthorizationState::RetirementPending {
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                last_failure: Some(DeviceCertificateRetirementError::OutcomeUnknown),
                ..
            }
        ));

        let acknowledgement = DeviceCertificateDeliveryAcknowledgement {
            authorization_id: delivery.authorization_id,
            device_code: start.device_code.clone(),
            delivery_id: delivery.delivery_id,
            certificate_sha256: delivery.certificate_sha256,
            csr_sha256: delivery.csr_sha256,
            csr_spki_sha256: delivery.csr_spki_sha256,
        };
        assert_eq!(
            manager.acknowledge_delivery(&acknowledgement, &ports),
            Err(DeviceAuthorizationError::CertificateRetirementPending {
                status: DeviceDeliveryRecoveryStatus::RevocationPending,
            })
        );

        ports.set_current_time(61_300);
        assert_eq!(
            manager.poll(&start.device_code, 61_300, &ports),
            Ok(DeviceAuthorizationPoll::DeliveryExpired {
                delivery_id,
                certificate_sha256,
            })
        );
        assert_eq!(ports.retirement_calls(), 3);
        assert!(matches!(
            store
                .by_approval_id(&challenge.approval_id)
                .expect("read expired delivery")
                .expect("approval record")
                .state,
            DeviceAuthorizationState::DeliveryExpired { .. }
        ));
    }

    #[test]
    fn retirement_worker_entrypoint_reserves_before_revoke_and_retries_unknown_result() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let (ports, _verifier_started) =
            CoordinatedApprovalPorts::new(VerifierBehavior::Allow, 300);
        let challenge = manager
            .begin_approval(&start.user_code, &scope(), &user(), &[26; 32], 200, &ports)
            .expect("challenge");
        manager
            .complete_approval(
                &challenge.approval_id,
                b"test-only-valid-assertion",
                300,
                &ports,
            )
            .expect("approval creates delivery");

        let delivery_deadline_unix_ms = match store
            .by_authorization_id(&start.authorization_id)
            .expect("read authorization")
            .expect("authorization")
            .state
        {
            DeviceAuthorizationState::DeliveryPending {
                delivery_deadline_unix_ms,
                ..
            } => delivery_deadline_unix_ms,
            state => panic!("unexpected state: {state:?}"),
        };
        assert_eq!(
            manager.retire_certificate_work_by_authorization_id(
                &start.authorization_id,
                delivery_deadline_unix_ms - 1,
                &ports,
            ),
            Ok(DeviceCertificateRetirementWorkOutcome::NoWork)
        );
        assert_eq!(ports.retirement_calls(), 0);

        ports.set_retirement_failures_remaining(1);
        assert_eq!(
            manager.retire_certificate_work_by_authorization_id(
                &start.authorization_id,
                delivery_deadline_unix_ms,
                &ports,
            ),
            Ok(DeviceCertificateRetirementWorkOutcome::Pending(
                DeviceDeliveryRecoveryStatus::RevocationPending,
            ))
        );
        let pending = store
            .by_authorization_id(&start.authorization_id)
            .expect("read retirement reservation")
            .expect("authorization");
        assert!(matches!(
            pending.state,
            DeviceAuthorizationState::RetirementPending {
                reason: DeviceCertificateRetirementReason::DeliveryDeadlineReached,
                entered_at_unix_ms,
                last_failure: Some(DeviceCertificateRetirementError::OutcomeUnknown),
                ..
            } if entered_at_unix_ms == delivery_deadline_unix_ms
        ));
        assert_eq!(ports.retirement_calls(), 1);

        assert_eq!(
            manager.retire_certificate_work_by_authorization_id(
                &start.authorization_id,
                delivery_deadline_unix_ms + 1,
                &ports,
            ),
            Ok(DeviceCertificateRetirementWorkOutcome::Retired)
        );
        assert!(matches!(
            store
                .by_authorization_id(&start.authorization_id)
                .expect("read terminal state")
                .expect("authorization")
                .state,
            DeviceAuthorizationState::DeliveryExpired { .. }
        ));
        assert_eq!(ports.retirement_calls(), 2);
        assert_eq!(
            manager.retire_certificate_work_by_authorization_id(
                &start.authorization_id,
                delivery_deadline_unix_ms + 2,
                &ports,
            ),
            Ok(DeviceCertificateRetirementWorkOutcome::NoWork)
        );
        assert_eq!(ports.retirement_calls(), 2);
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
            manager.poll(&start.device_code, 60_100, &TestApprovalPorts),
            Err(DeviceAuthorizationError::Expired)
        );
    }

    #[test]
    fn start_rejects_directory_binding_with_mismatched_csr_digest() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let request_scope = scope();
        let csr_der = b"test-only-csr".to_vec();
        let mut binding = test_registration_binding(&request_scope, &csr_der, 900);
        binding = DeviceAuthorizationRegistrationBinding::test_fixture(
            *binding.binding_id(),
            binding.key().clone(),
            binding.authorization_generation(),
            [0; 32],
            *binding.spki_sha256(),
        );

        assert_eq!(
            manager.begin(
                DeviceAuthorizationRequest {
                    scope: request_scope,
                    csr_der,
                    registration_key_digest: DeviceRegistrationKeyDigest::from_secret(&[9; 32]),
                    registration_binding: binding,
                },
                100,
                &TestCsrValidator,
            ),
            Err(DeviceAuthorizationError::InvalidRegistrationBinding)
        );
        assert!(store.records.lock().expect("store lock").is_empty());
    }

    #[test]
    fn legacy_store_defaults_fail_closed_for_registered_start() {
        let inner = InMemoryDeviceAuthorizationStore::default();
        let manager = DeviceAuthorizationManager::new(
            LegacyDeviceAuthorizationStore(inner.clone()),
            InMemoryUserCodeAttemptLimiter::default(),
            test_user_code_key_ring(),
            test_policy(),
        )
        .expect("valid test policy");
        let request_scope = scope();
        let csr_der = b"test-only-csr".to_vec();

        assert_eq!(
            manager.begin(
                DeviceAuthorizationRequest {
                    scope: request_scope.clone(),
                    csr_der: csr_der.clone(),
                    registration_key_digest: DeviceRegistrationKeyDigest::from_secret(&[10; 32]),
                    registration_binding: test_registration_binding(&request_scope, &csr_der, 901,),
                },
                100,
                &TestCsrValidator,
            ),
            Err(DeviceAuthorizationError::StorageUnavailable)
        );
        assert!(inner.records.lock().expect("store lock").is_empty());
    }

    #[test]
    fn atomic_registration_recovery_store_defaults_fail_closed() {
        let inner = InMemoryDeviceAuthorizationStore::default();
        let manager = DeviceAuthorizationManager::new(
            LegacyDeviceAuthorizationStore(inner.clone()),
            InMemoryUserCodeAttemptLimiter::default(),
            test_user_code_key_ring(),
            test_policy(),
        )
        .expect("valid test policy");
        let csr_der = b"test-only-csr".to_vec();
        let recovery_secret = [0x31; 32];
        let digest = DeviceRegistrationKeyDigest::from_secret(&recovery_secret);
        assert!(format!("{digest:?}").contains("REDACTED"));
        assert!(!format!("{digest:?}").contains("49"));

        let request = DeviceAuthorizationRegistrationRequest::new(
            scope(),
            csr_der.clone(),
            sha256(&csr_der),
            [1; 32],
            digest,
        );
        assert_eq!(
            manager.begin_or_recover_registered(request, 100, &TestCsrValidator),
            Err(DeviceAuthorizationError::StorageUnavailable)
        );
        assert!(inner.records.lock().expect("store lock").is_empty());
    }

    #[test]
    fn poll_snapshot_is_fenced_by_current_code_and_revision() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(store.clone());
        let start = start(&manager);
        let code_hash = device_code_hash(&start.device_code).expect("valid start code");
        let initial = store
            .current_poll_snapshot(&code_hash, None, 200)
            .expect("poll snapshot")
            .expect("current code");
        assert_eq!(initial.record.id, start.authorization_id);
        assert_eq!(
            initial.record.device_code_generation,
            start.device_code_generation
        );
        assert_eq!(initial.database_now_unix_ms, 200);
        assert!(store
            .current_poll_snapshot(&code_hash, Some(initial.record.revision + 1), 200,)
            .expect("stale poll revision")
            .is_none());

        assert_eq!(
            manager.poll(&start.device_code, 1_100, &TestApprovalPorts),
            Ok(DeviceAuthorizationPoll::Pending { interval_ms: 1_000 })
        );
        let current = store
            .current_poll_snapshot(&code_hash, Some(initial.record.revision + 1), 1_100)
            .expect("committed poll revision")
            .expect("current poll projection");
        assert_eq!(current.record.device_code_generation, 1);
        assert_eq!(current.record.last_poll_at_unix_ms, Some(1_100));
    }

    #[test]
    fn legacy_store_defaults_fail_closed_for_registered_poll_and_ack() {
        let inner = InMemoryDeviceAuthorizationStore::default();
        let manager = manager_with_store(inner.clone());
        let start = start(&manager);
        let challenge = manager
            .begin_approval(
                &start.user_code,
                &scope(),
                &user(),
                &[28; 32],
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
            .expect("issue fake test certificate");
        let delivery = match manager
            .poll(&start.device_code, 1_100, &TestApprovalPorts)
            .expect("test delivery")
        {
            DeviceAuthorizationPoll::CertificateReady(delivery) => delivery,
            result => panic!("unexpected poll result: {result:?}"),
        };

        let legacy_manager = DeviceAuthorizationManager::new(
            LegacyDeviceAuthorizationStore(inner),
            InMemoryUserCodeAttemptLimiter::default(),
            test_user_code_key_ring(),
            test_policy(),
        )
        .expect("valid test policy");
        assert_eq!(
            legacy_manager.poll(&start.device_code, 2_200, &TestApprovalPorts),
            Err(DeviceAuthorizationError::StorageUnavailable)
        );
        assert_eq!(
            legacy_manager.acknowledge_delivery(
                &DeviceCertificateDeliveryAcknowledgement {
                    authorization_id: delivery.authorization_id,
                    device_code: start.device_code,
                    delivery_id: delivery.delivery_id,
                    certificate_sha256: delivery.certificate_sha256,
                    csr_sha256: delivery.csr_sha256,
                    csr_spki_sha256: delivery.csr_spki_sha256,
                },
                &TestApprovalPorts,
            ),
            Err(DeviceAuthorizationError::StorageUnavailable)
        );
    }

    #[test]
    fn early_poll_slowdown_increases_the_required_interval() {
        let manager = manager();
        let start = start(&manager);
        assert_eq!(
            manager.poll(&start.device_code, 200, &TestApprovalPorts),
            Ok(DeviceAuthorizationPoll::Pending { interval_ms: 1_000 })
        );
        assert_eq!(
            manager.poll(&start.device_code, 201, &TestApprovalPorts),
            Err(DeviceAuthorizationError::SlowDown {
                retry_after_ms: 1_999,
            })
        );
        assert_eq!(
            manager.poll(&start.device_code, 2_200, &TestApprovalPorts),
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
            manager.poll(&start.device_code, 203, &TestApprovalPorts),
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
            manager.poll(&start.device_code, 1_200, ports.as_ref()),
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
            DeviceAuthorizationState::DeliveryPending { .. }
        ));
    }

    #[test]
    fn newer_directory_generation_fences_old_approval_before_signer_call() {
        let store = InMemoryDeviceAuthorizationStore::default();
        let manager = Arc::new(manager_with_store(store));
        let old_start = start_with_generation(manager.as_ref(), 700);
        let (ports, verifier_started) = CoordinatedApprovalPorts::new(VerifierBehavior::Block, 300);
        let ports = Arc::new(ports);
        let challenge = manager
            .begin_approval(
                &old_start.user_code,
                &scope(),
                &user(),
                &[26; 32],
                200,
                ports.as_ref(),
            )
            .expect("old generation challenge");

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
            .expect("old generation verifier entered");

        let new_start = start_with_generation(manager.as_ref(), 701);
        assert_eq!(new_start.device_id, old_start.device_id);
        assert!(new_start.authorization_generation > old_start.authorization_generation);
        ports.release_verifier();

        assert_eq!(
            approval.join().expect("approval thread"),
            Err(DeviceAuthorizationError::ConcurrentTransition)
        );
        assert_eq!(ports.issuer_calls(), 0);
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
            manager.poll(&start.device_code, 60_100, ports.as_ref()),
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
