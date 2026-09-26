//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_enrollment_http.rs                                       │
//! │  Module: cy_workspace_fabric::device_enrollment_http                │
//! │  Role: Fail-closed HTTP routes for Cyrene device enrollment v1.     │
//! │                                                                     │
//! │  模块职责：提供设备注册 v1 HTTP 路由与可信身份注入边界。               │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use cy_proto::workspace_v1::UserIdentityRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroize;

use crate::device_authorization::{
    device_code_hash, DeviceAuthorizationCommittedSnapshot, DeviceAuthorizationPollSnapshot,
    DeviceAuthorizationPortError, DeviceAuthorizationRegistrationRequest, DeviceAuthorizationScope,
    DeviceAuthorizationStartDisposition, DeviceAuthorizationState, DeviceCsrValidator,
    DeviceRegistrationKeyDigest,
};

const MAX_CSR_DER_BYTES: usize = 16 * 1024;
const MAX_WEBAUTHN_ASSERTION_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 128 * 1024;

/// Directory-owned identity mapping for one registration binding.
///
/// The Directory adapter must derive device_id from its own stable identity
/// authority. This value is passed to the authorization application; neither
/// this module nor the manager may derive it from authorization_id.
#[derive(Clone, PartialEq, Eq)]
pub struct DirectoryRegistrationBinding {
    /// Opaque Directory binding record identifier.
    pub binding_id: [u8; 16],
    /// Stable Directory-assigned device identifier, never derived from the authorization ID.
    pub device_id: String,
    /// Exact organization and workspace scope in the binding.
    pub scope: DeviceAuthorizationScope,
    /// Per-device generation for a new authorization or certificate rotation.
    pub authorization_generation: u64,
    /// SHA-256 digest of the exact CSR DER bytes.
    pub csr_sha256: [u8; 32],
    /// SHA-256 digest of the CSR SubjectPublicKeyInfo.
    pub spki_sha256: [u8; 32],
}

/// Redacted, non-serializable projection of one committed authorization view.
/// The device-code hash remains private and only fences the response to the
/// exact bearer code that produced this database snapshot.
pub struct DeviceEnrollmentAuthorizationSnapshot {
    authorization_id: String,
    binding_id: [u8; 16],
    device_id: String,
    scope: DeviceAuthorizationScope,
    authorization_generation: u64,
    device_code_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    revision: u64,
    expires_at_unix_ms: u64,
    device_code_hash: [u8; 32],
    start_disposition: Option<DeviceAuthorizationStartDisposition>,
    poll_state: DeviceEnrollmentPollState,
}

#[cfg(test)]
struct DeviceEnrollmentAuthorizationSnapshotFixture<'a> {
    authorization_id: String,
    binding_id: [u8; 16],
    device_id: String,
    scope: DeviceAuthorizationScope,
    authorization_generation: u64,
    device_code_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    revision: u64,
    expires_at_unix_ms: u64,
    device_code: &'a str,
    poll_state: DeviceEnrollmentPollState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceEnrollmentPollState {
    Pending,
    DeliveryPending,
    Delivered,
    RetirementPending,
    DeliveryExpired,
    Denied,
    Expired,
    IssuanceFailed,
    Superseded,
    RegistrationRetired,
}

impl DeviceEnrollmentAuthorizationSnapshot {
    /// Projects a successful start or recovery without exposing the full
    /// record, user-code MAC, CSR bytes, or opaque WebAuthn state.
    pub fn from_start_snapshot(
        snapshot: &DeviceAuthorizationCommittedSnapshot,
    ) -> Result<Self, DeviceEnrollmentHttpError> {
        if snapshot.record.registration_key_digest.is_none() {
            return Err(DeviceEnrollmentHttpError::Unavailable);
        }
        let mut projection = Self::from_record(&snapshot.record)?;
        projection.start_disposition = Some(snapshot.disposition);
        Ok(projection)
    }

    /// Projects the manager's final poll revision fence. Legacy records remain
    /// pollable but cannot be recovered by registration key.
    pub fn from_poll_snapshot(
        snapshot: &DeviceAuthorizationPollSnapshot,
    ) -> Result<Self, DeviceEnrollmentHttpError> {
        Self::from_record(&snapshot.record)
    }

    fn from_record(
        record: &crate::device_authorization::DeviceAuthorizationRecord,
    ) -> Result<Self, DeviceEnrollmentHttpError> {
        let binding = &record.registration_binding;
        if record.device_code_generation == 0
            || binding.authorization_generation() == 0
            || binding.key().device_id.trim().is_empty()
        {
            return Err(DeviceEnrollmentHttpError::Unavailable);
        }
        Ok(Self {
            authorization_id: URL_SAFE_NO_PAD.encode(record.id),
            binding_id: *binding.binding_id(),
            device_id: binding.key().device_id.clone(),
            scope: record.scope.clone(),
            authorization_generation: binding.authorization_generation(),
            device_code_generation: record.device_code_generation,
            csr_sha256: record.csr_sha256,
            spki_sha256: record.spki_sha256,
            revision: record.revision,
            expires_at_unix_ms: record.expires_at_unix_ms,
            device_code_hash: record.device_code_hash,
            start_disposition: None,
            poll_state: enrollment_poll_state(&record.state),
        })
    }

    fn matches_device_code(&self, device_code: &str) -> bool {
        device_code_hash(device_code).is_ok_and(|hash| hash == self.device_code_hash)
    }

    #[cfg(test)]
    fn test_fixture(fixture: DeviceEnrollmentAuthorizationSnapshotFixture<'_>) -> Self {
        Self {
            authorization_id: fixture.authorization_id,
            binding_id: fixture.binding_id,
            device_id: fixture.device_id,
            scope: fixture.scope,
            authorization_generation: fixture.authorization_generation,
            device_code_generation: fixture.device_code_generation,
            csr_sha256: fixture.csr_sha256,
            spki_sha256: fixture.spki_sha256,
            revision: fixture.revision,
            expires_at_unix_ms: fixture.expires_at_unix_ms,
            device_code_hash: device_code_hash(fixture.device_code)
                .expect("valid test device code"),
            start_disposition: None,
            poll_state: fixture.poll_state,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn expires_at_unix_ms(&self) -> u64 {
        self.expires_at_unix_ms
    }
}

/// Complete result of one atomic Directory binding and authorization start or
/// same-authorization recovery.
pub struct DeviceEnrollmentStartResult {
    /// Authoritative identity tuple returned by Directory.
    pub binding: DirectoryRegistrationBinding,
    /// Current codes and authorization reference to return to the device.
    pub response: StartDeviceAuthorizationResponse,
    /// Exact committed state snapshot from which this response was projected.
    pub committed_snapshot: DeviceEnrollmentAuthorizationSnapshot,
}

/// Transaction boundary for Directory identity binding and authorization
/// state. Implementations must compose the Directory registration key digest,
/// stable device ID and generation, authorization record, and device-code
/// digest/generation under one durable transaction or equivalent shared lock.
///
/// A matching recovery key and exact scope/CSR/SPKI tuple must retain the same
/// authorization and CA idempotency key, rotate codes with a revision CAS, and
/// fence poll/ACK against the superseded code. `ISSUING` and `DELIVERY_PENDING`
/// recover the same issuance/delivery. Each successful same-authorization code
/// rotation increments `device_code_generation` while preserving
/// `authorization_generation`. Recovery must enforce the original authorization
/// TTL, attempt limits, and terminal-state conflicts. Missing this composite
/// transaction is a production blocker; the HTTP dependency must remain absent
/// and start returns 503.
#[async_trait]
pub trait DeviceEnrollmentRegistrationTransactionPort: Send + Sync {
    /// Atomically binds or resolves the Directory identity and starts or
    /// recovers the matching authorization.
    async fn bind_and_start(
        &self,
        request: DeviceAuthorizationRegistrationRequest,
    ) -> Result<DeviceEnrollmentStartResult, DeviceEnrollmentHttpError>;
}

/// Secret string wrapper that clears its buffer when the parsed request drops.
/// It intentionally does not implement Debug or Serialize.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Zeroizing 256-bit registration recovery credential.
pub struct RecoveryCredential([u8; 32]);

impl RecoveryCredential {
    /// Borrows the raw 256-bit value so the transaction adapter can compute a
    /// domain-separated digest. Implementations must never persist or log it.
    pub fn bytes_for_digest(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for RecoveryCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Zeroizing bytes for a WebAuthn assertion forwarded to the verifier port.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Borrows the assertion bytes for verification.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Trusted user session inserted by the host authentication middleware.
///
/// Only the verified interactive identity boundary may construct this request
/// extension. User identity is never accepted from JSON request fields.
#[derive(Clone)]
pub struct TrustedInteractiveUserSession {
    identity: UserIdentityRef,
}

impl TrustedInteractiveUserSession {
    /// Creates the extension from issuer and subject values already verified by
    /// the configured interactive identity provider.
    pub fn from_verified_identity(
        issuer: String,
        subject: String,
    ) -> Result<Self, DeviceEnrollmentHttpError> {
        if issuer.trim().is_empty() || subject.trim().is_empty() {
            return Err(DeviceEnrollmentHttpError::InvalidRequest);
        }
        Ok(Self {
            identity: UserIdentityRef { issuer, subject },
        })
    }
}

/// Abuse key derived by trusted middleware from the pre-auth session and
/// request source. Do not construct it from raw user input or log its bytes.
#[derive(Clone)]
pub struct TrustedEnrollmentAbuseKey([u8; 32]);

impl TrustedEnrollmentAbuseKey {
    /// Creates the key from a privacy-preserving server-derived digest.
    pub fn from_server_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

/// Application boundary backed by DeviceAuthorizationManager and injected
/// membership, WebAuthn, CA, registry activation, delivery-retirement, and
/// durable storage ports.
///
/// This is deliberately a port rather than a pretend production adapter:
/// current Directory/state APIs do not yet provide production atomic
/// registration-key recovery, registry activation, or host composition. Hosts
/// must leave this port absent until those adapters exist, which makes the
/// protected flows return 503.
#[async_trait]
pub trait DeviceEnrollmentAuthorizationPort: Send + Sync {
    /// Starts a membership-checked WebAuthn approval attempt.
    ///
    /// The trusted identity and exact request scope come from this handler;
    /// the adapter rechecks membership and binds verifier options/state to the
    /// stored approval and CSR/SPKI digests.
    async fn begin_approval(
        &self,
        user: &UserIdentityRef,
        abuse_key: &[u8; 32],
        user_code: &str,
        scope: DeviceAuthorizationScope,
    ) -> Result<Value, DeviceEnrollmentHttpError>;

    /// Completes the stored WebAuthn ceremony. The assertion remains in memory
    /// only and must never be logged or persisted as raw JSON.
    ///
    /// Implementations must verify the trusted identity matches the stored
    /// approver, resume only the exact persisted approval, and reserve `ISSUING`
    /// before calling the idempotent CA issuer.
    async fn complete_approval(
        &self,
        user: &UserIdentityRef,
        approval_id: String,
        assertion_json: Option<SecretBytes>,
    ) -> Result<CompleteApprovalHttpResponse, DeviceEnrollmentHttpError>;

    /// Denies an authorization after trusted identity and membership checks.
    ///
    /// Denial is membership-checked in the exact supplied scope and does not
    /// require WebAuthn because it cannot grant a device credential.
    async fn deny(
        &self,
        user: &UserIdentityRef,
        abuse_key: &[u8; 32],
        user_code: &str,
        scope: DeviceAuthorizationScope,
    ) -> Result<Value, DeviceEnrollmentHttpError>;

    /// Polls through the durable manager and delivery-retirement ports.
    ///
    /// The adapter authenticates only the current device code, resolves the
    /// authoritative Directory identity and certificate projection, and
    /// rechecks the code revision before returning a delivery.
    async fn poll(&self, device_code: &str) -> Result<PollHttpResponse, DeviceEnrollmentHttpError>;

    /// Verifies and commits the exact delivery acknowledgement idempotently.
    ///
    /// The manager must compare authorization, current device code, delivery,
    /// certificate fingerprint, CSR digest, and SPKI digest in its durable CAS.
    async fn acknowledge_delivery(
        &self,
        request: AcknowledgeDeviceDeliveryCommand,
    ) -> Result<Value, DeviceEnrollmentHttpError>;
}

/// Routes return 200 after a completed/recovered approval and 202 while the
/// same durable CA issuance remains in progress.
pub struct CompleteApprovalHttpResponse {
    /// OpenAPI-shaped approval result containing no assertion or opaque state.
    pub body: Value,
    /// True once certificate issuance/delivery state is durably recoverable.
    pub accepted: bool,
}

/// Poll payload with the contract-mandated HTTP status mapping.
pub struct PollHttpResponse {
    /// OpenAPI-shaped pending, terminal, or certificate delivery result.
    pub body: Value,
    /// Determines whether this is an approved 200 or RFC-style 400 outcome.
    pub status: PollHttpStatus,
    /// Seconds for the HTTP `Retry-After` header on `slow_down` responses.
    pub retry_after_seconds: Option<u64>,
    /// Exact current authorization projection used to produce `body`.
    pub committed_snapshot: DeviceEnrollmentAuthorizationSnapshot,
}

/// HTTP outcome class for RFC 8628-style polling.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PollHttpStatus {
    /// A certificate delivery passed the response binding checks.
    Approved,
    /// A pending or terminal RFC device authorization outcome.
    ProtocolError,
}

/// Exact ACK tuple after syntactic and digest-length validation.
pub struct AcknowledgeDeviceDeliveryCommand {
    /// ID of the authorization whose certificate was delivered.
    pub authorization_id: String,
    /// Bearer code proving possession by the enrolling device.
    pub device_code: SecretString,
    /// Stable ID for the exact certificate delivery attempt.
    pub delivery_id: String,
    /// SHA-256 of the certificate DER received by the device.
    pub certificate_sha256: [u8; 32],
    /// SHA-256 of the CSR public key.
    pub csr_spki_sha256: [u8; 32],
    /// SHA-256 of the exact CSR DER bytes.
    pub csr_sha256: [u8; 32],
}

/// Application errors map to generic non-secret API errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeviceEnrollmentHttpError {
    /// Request syntax, scope, code format, or contract shape is invalid.
    #[error("invalid enrollment request")]
    InvalidRequest,
    /// Trusted interactive user session is missing.
    #[error("authentication required")]
    Unauthorized,
    /// Trusted user does not have access to the exact scope.
    #[error("scope access denied")]
    Forbidden,
    /// Authorization, code, or delivery does not exist for this request.
    #[error("authorization or code not found")]
    InvalidGrant,
    /// State is terminal, stale, or conflicts with the exact request tuple.
    #[error("authorization state conflict")]
    Conflict,
    /// Authorization or delivery deadline passed.
    #[error("authorization or delivery expired")]
    Expired,
    /// An attempt limit was reached; clients must wait for the supplied interval.
    #[error("request rate limit exceeded")]
    RateLimited { retry_after_seconds: u64 },
    /// A required Directory, store, verifier, CA, retirement, or registry port is absent.
    #[error("required enrollment service is unavailable")]
    Unavailable,
}

/// Injected HTTP dependencies. Missing Directory, CSR, or authorization/CA
/// adapters are represented as None and fail closed with 503.
#[derive(Clone, Default)]
pub struct DeviceEnrollmentHttpDependencies {
    /// Atomic Directory registration and manager recovery transaction.
    pub registration: Option<Arc<dyn DeviceEnrollmentRegistrationTransactionPort>>,
    /// CSR parser and proof-of-possession validator.
    pub csr_validator: Option<Arc<dyn DeviceCsrValidator>>,
    /// Manager adapter with membership, WebAuthn, CA, registry, and delivery ports.
    pub authorization: Option<Arc<dyn DeviceEnrollmentAuthorizationPort>>,
}

/// Creates the device enrollment v1 routes with explicit trust-boundary ports.
pub fn device_enrollment_v1_router(dependencies: DeviceEnrollmentHttpDependencies) -> Router {
    let state = DeviceEnrollmentHttpState { dependencies };
    Router::new()
        .route(
            "/v1/device-authorizations",
            post(start_device_authorization),
        )
        .route(
            "/v1/device-authorizations/approval-challenges",
            post(begin_device_approval),
        )
        .route(
            "/v1/device-authorizations/approval-challenges/:approval_id/complete",
            post(complete_device_approval),
        )
        .route(
            "/v1/device-authorizations/denials",
            post(deny_device_authorization),
        )
        .route(
            "/v1/device-authorizations/poll",
            post(poll_device_authorization),
        )
        .route(
            "/v1/device-authorizations/delivery-acknowledgements",
            post(acknowledge_device_delivery),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

#[derive(Clone)]
struct DeviceEnrollmentHttpState {
    dependencies: DeviceEnrollmentHttpDependencies,
}

/// Exact tenant scope in the public JSON contract.
#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceScopeWire {
    /// Exact target organization ID.
    pub organization_id: String,
    /// Exact target workspace ID.
    pub workspace_id: String,
}

impl DeviceScopeWire {
    fn into_domain(self) -> Result<DeviceAuthorizationScope, DeviceEnrollmentHttpError> {
        if self.organization_id.trim().is_empty()
            || self.workspace_id.trim().is_empty()
            || self.organization_id.len() > 256
            || self.workspace_id.len() > 256
        {
            return Err(DeviceEnrollmentHttpError::InvalidRequest);
        }
        Ok(DeviceAuthorizationScope {
            organization_id: self.organization_id,
            workspace_id: self.workspace_id,
        })
    }
}

/// Public references generated by the application only after Directory
/// binding. IDs are separate opaque values and neither is an authenticator.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorizationReferenceWire {
    /// Opaque ID for this authorization attempt.
    pub authorization_id: String,
    /// Stable Directory identity for the WorkspaceDevice.
    pub device_id: String,
    /// Exact target scope approved for the certificate.
    pub scope: DeviceScopeWire,
    /// Base64-encoded SHA-256 of the CSR SPKI.
    pub csr_spki_sha256: String,
    /// Base64-encoded SHA-256 of the exact CSR DER bytes.
    pub csr_sha256: String,
    /// RFC 3339 expiration timestamp.
    pub expires_at: String,
    /// Per-device generation for new authorization or certificate rotation.
    pub authorization_generation: u64,
}

/// Current device/user codes. Recovery responses increment the code generation
/// without changing the authorization generation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorizationCodesWire {
    /// 256-bit bearer secret returned only for the current start generation.
    pub device_code: String,
    /// Human-readable, rate-limited authorization lookup code.
    pub user_code: String,
    /// Base verification URI with no embedded user code.
    pub verification_uri: String,
    /// Optional convenience URI that may contain the user code and is sensitive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    /// Minimum polling interval in seconds.
    pub interval_seconds: u32,
    /// RFC 3339 expiration timestamp.
    pub expires_at: String,
    /// Same-authorization code recovery revision.
    pub device_code_generation: u64,
}

impl Drop for DeviceAuthorizationCodesWire {
    fn drop(&mut self) {
        self.device_code.zeroize();
        self.user_code.zeroize();
        if let Some(uri) = &mut self.verification_uri_complete {
            uri.zeroize();
        }
    }
}

/// Start response with a Directory-owned device identity and one authorization.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartDeviceAuthorizationResponse {
    /// Directory-owned device and authorization reference.
    pub authorization: DeviceAuthorizationReferenceWire,
    /// Current one-time codes and verification details.
    pub codes: DeviceAuthorizationCodesWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartDeviceAuthorizationRequestWire {
    scope: DeviceScopeWire,
    csr_der: String,
    csr_spki_sha256: String,
    registration_key: SecretString,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginDeviceApprovalRequestWire {
    user_code: SecretString,
    scope: DeviceScopeWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteDeviceApprovalRequestWire {
    #[serde(default)]
    webauthn_assertion: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DenyDeviceAuthorizationRequestWire {
    user_code: SecretString,
    scope: DeviceScopeWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PollDeviceAuthorizationRequestWire {
    device_code: SecretString,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcknowledgeDeviceDeliveryRequestWire {
    authorization_id: String,
    device_code: SecretString,
    delivery_id: String,
    certificate_sha256: String,
    csr_spki_sha256: String,
    csr_sha256: String,
}

async fn start_device_authorization(
    State(state): State<DeviceEnrollmentHttpState>,
    payload: Result<
        Json<StartDeviceAuthorizationRequestWire>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<(StatusCode, Json<StartDeviceAuthorizationResponse>), HttpApiFailure> {
    let Json(request) = parse_json(payload)?;
    let scope = request.scope.into_domain()?;
    let csr_der = STANDARD
        .decode(request.csr_der)
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)?;
    if csr_der.is_empty() || csr_der.len() > MAX_CSR_DER_BYTES {
        return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
    }
    let declared_spki_sha256 = decode_sha256(&request.csr_spki_sha256)?;
    let validator = state
        .dependencies
        .csr_validator
        .as_ref()
        .ok_or(DeviceEnrollmentHttpError::Unavailable)?;
    let actual_spki_sha256 = validator
        .validate_and_hash_spki(&csr_der)
        .map_err(map_csr_error)?;
    if actual_spki_sha256 != declared_spki_sha256 {
        return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
    }
    let csr_sha256: [u8; 32] = Sha256::digest(&csr_der).into();
    let recovery_key_bytes = STANDARD
        .decode(request.registration_key.expose())
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)?;
    let recovery_key: [u8; 32] = match recovery_key_bytes.try_into() {
        Ok(recovery_key) => recovery_key,
        Err(mut invalid_length_key) => {
            invalid_length_key.zeroize();
            return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
        }
    };
    let recovery_credential = RecoveryCredential(recovery_key);
    let registration_key_digest =
        DeviceRegistrationKeyDigest::from_secret(recovery_credential.bytes_for_digest());
    drop(recovery_credential);

    let registration = state
        .dependencies
        .registration
        .as_ref()
        .ok_or(DeviceEnrollmentHttpError::Unavailable)?;
    let start = registration
        .bind_and_start(DeviceAuthorizationRegistrationRequest::new(
            scope.clone(),
            csr_der,
            csr_sha256,
            actual_spki_sha256,
            registration_key_digest,
        ))
        .await?;
    validate_directory_binding(&start.binding, &scope, &csr_sha256, &actual_spki_sha256)?;
    validate_start_response(&start.response, &start.binding)?;
    validate_start_commit(&start.committed_snapshot, &start.binding, &start.response)?;
    Ok((StatusCode::CREATED, Json(start.response)))
}

async fn begin_device_approval(
    State(state): State<DeviceEnrollmentHttpState>,
    user: Option<Extension<TrustedInteractiveUserSession>>,
    abuse_key: Option<Extension<TrustedEnrollmentAbuseKey>>,
    payload: Result<Json<BeginDeviceApprovalRequestWire>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpApiFailure> {
    let user = trusted_user(user)?;
    let abuse_key = trusted_abuse_key(abuse_key)?;
    let Json(request) = parse_json(payload)?;
    validate_user_code(request.user_code.expose())?;
    let scope = request.scope.into_domain()?;
    let authorization = required_authorization(&state)?;
    let body = authorization
        .begin_approval(&user, &abuse_key, request.user_code.expose(), scope.clone())
        .await?;
    validate_approval_challenge_response(&body, &scope)?;
    ensure_no_secret_fields(&body, false)?;
    Ok((StatusCode::OK, Json(body)))
}

async fn complete_device_approval(
    State(state): State<DeviceEnrollmentHttpState>,
    Path(approval_id): Path<String>,
    user: Option<Extension<TrustedInteractiveUserSession>>,
    payload: Result<
        Json<CompleteDeviceApprovalRequestWire>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Response, HttpApiFailure> {
    let user = trusted_user(user)?;
    if approval_id.len() < 16 || approval_id.len() > 128 {
        return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
    }
    let Json(request) = parse_json(payload)?;
    let assertion_json = request
        .webauthn_assertion
        .map(serialize_webauthn_assertion)
        .transpose()
        .map_err(HttpApiFailure::from)?;
    if assertion_json.as_ref().is_some_and(|assertion| {
        assertion.expose().is_empty() || assertion.expose().len() > MAX_WEBAUTHN_ASSERTION_BYTES
    }) {
        return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
    }
    let authorization = required_authorization(&state)?;
    let response = authorization
        .complete_approval(&user, approval_id, assertion_json)
        .await?;
    validate_complete_approval_response(&response.body, &user, response.accepted)?;
    ensure_no_secret_fields(&response.body, false)?;
    let status = if response.accepted {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(response.body)).into_response())
}

async fn deny_device_authorization(
    State(state): State<DeviceEnrollmentHttpState>,
    user: Option<Extension<TrustedInteractiveUserSession>>,
    abuse_key: Option<Extension<TrustedEnrollmentAbuseKey>>,
    payload: Result<
        Json<DenyDeviceAuthorizationRequestWire>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<(StatusCode, Json<Value>), HttpApiFailure> {
    let user = trusted_user(user)?;
    let abuse_key = trusted_abuse_key(abuse_key)?;
    let Json(request) = parse_json(payload)?;
    validate_user_code(request.user_code.expose())?;
    let scope = request.scope.into_domain()?;
    let authorization = required_authorization(&state)?;
    let body = authorization
        .deny(&user, &abuse_key, request.user_code.expose(), scope.clone())
        .await?;
    validate_denial_response(&body, &user, &scope)?;
    ensure_no_secret_fields(&body, false)?;
    Ok((StatusCode::OK, Json(body)))
}

async fn poll_device_authorization(
    State(state): State<DeviceEnrollmentHttpState>,
    payload: Result<
        Json<PollDeviceAuthorizationRequestWire>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Response, HttpApiFailure> {
    let Json(request) = parse_json(payload)?;
    validate_device_code(request.device_code.expose())?;
    let authorization = required_authorization(&state)?;
    let response = authorization.poll(request.device_code.expose()).await?;
    validate_poll_response(&response, request.device_code.expose())?;
    ensure_no_secret_fields(&response.body, false)?;
    let status = match response.status {
        PollHttpStatus::Approved => StatusCode::OK,
        PollHttpStatus::ProtocolError => StatusCode::BAD_REQUEST,
    };
    let mut http_response = (status, Json(response.body)).into_response();
    if let Some(seconds) = response.retry_after_seconds {
        let value = HeaderValue::from_str(&seconds.max(1).to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("1"));
        http_response
            .headers_mut()
            .insert(header::RETRY_AFTER, value);
    }
    Ok(http_response)
}

async fn acknowledge_device_delivery(
    State(state): State<DeviceEnrollmentHttpState>,
    payload: Result<
        Json<AcknowledgeDeviceDeliveryRequestWire>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<(StatusCode, Json<Value>), HttpApiFailure> {
    let Json(request) = parse_json(payload)?;
    if !is_canonical_authorization_id(&request.authorization_id)
        || request.delivery_id.len() < 16
        || request.delivery_id.len() > 128
    {
        return Err(DeviceEnrollmentHttpError::InvalidRequest.into());
    }
    validate_device_code(request.device_code.expose())?;
    let command = AcknowledgeDeviceDeliveryCommand {
        authorization_id: request.authorization_id,
        device_code: request.device_code,
        delivery_id: request.delivery_id,
        certificate_sha256: decode_sha256(&request.certificate_sha256)?,
        csr_spki_sha256: decode_sha256(&request.csr_spki_sha256)?,
        csr_sha256: decode_sha256(&request.csr_sha256)?,
    };
    let expected_authorization_id = command.authorization_id.clone();
    let expected_delivery_id = command.delivery_id.clone();
    let expected_certificate_sha256 = command.certificate_sha256;
    let authorization = required_authorization(&state)?;
    let body = authorization.acknowledge_delivery(command).await?;
    validate_acknowledgement_response(
        &body,
        &expected_authorization_id,
        &expected_delivery_id,
        &expected_certificate_sha256,
    )?;
    ensure_no_secret_fields(&body, false)?;
    Ok((StatusCode::OK, Json(body)))
}

fn required_authorization(
    state: &DeviceEnrollmentHttpState,
) -> Result<&Arc<dyn DeviceEnrollmentAuthorizationPort>, HttpApiFailure> {
    state
        .dependencies
        .authorization
        .as_ref()
        .ok_or_else(|| DeviceEnrollmentHttpError::Unavailable.into())
}

fn trusted_user(
    user: Option<Extension<TrustedInteractiveUserSession>>,
) -> Result<UserIdentityRef, HttpApiFailure> {
    user.map(|Extension(session)| session.identity)
        .ok_or_else(|| DeviceEnrollmentHttpError::Unauthorized.into())
}

fn trusted_abuse_key(
    key: Option<Extension<TrustedEnrollmentAbuseKey>>,
) -> Result<[u8; 32], HttpApiFailure> {
    key.map(|Extension(key)| key.0)
        .ok_or_else(|| DeviceEnrollmentHttpError::Unavailable.into())
}

fn parse_json<T>(
    payload: Result<Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<T>, HttpApiFailure> {
    payload.map_err(|_| DeviceEnrollmentHttpError::InvalidRequest.into())
}

fn decode_sha256(value: &str) -> Result<[u8; 32], DeviceEnrollmentHttpError> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)?;
    bytes
        .try_into()
        .map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)
}

fn serialize_webauthn_assertion(
    mut assertion: Value,
) -> Result<SecretBytes, DeviceEnrollmentHttpError> {
    let serialized = serde_json::to_vec(&assertion).map(SecretBytes);
    zeroize_json_strings(&mut assertion);
    serialized.map_err(|_| DeviceEnrollmentHttpError::InvalidRequest)
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_strings),
        Value::Object(values) => values.values_mut().for_each(zeroize_json_strings),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn map_csr_error(error: DeviceAuthorizationPortError) -> DeviceEnrollmentHttpError {
    match error {
        DeviceAuthorizationPortError::Rejected => DeviceEnrollmentHttpError::InvalidRequest,
        DeviceAuthorizationPortError::Unavailable => DeviceEnrollmentHttpError::Unavailable,
    }
}

fn validate_directory_binding(
    binding: &DirectoryRegistrationBinding,
    scope: &DeviceAuthorizationScope,
    csr_sha256: &[u8; 32],
    spki_sha256: &[u8; 32],
) -> Result<(), HttpApiFailure> {
    if binding.device_id.trim().is_empty()
        || binding.device_id.len() > 128
        || binding.authorization_generation == 0
        || binding.scope != *scope
        || binding.csr_sha256 != *csr_sha256
        || binding.spki_sha256 != *spki_sha256
    {
        return Err(DeviceEnrollmentHttpError::Unavailable.into());
    }
    Ok(())
}

fn validate_start_response(
    response: &StartDeviceAuthorizationResponse,
    binding: &DirectoryRegistrationBinding,
) -> Result<(), HttpApiFailure> {
    let authorization = &response.authorization;
    if !is_canonical_authorization_id(&authorization.authorization_id)
        || authorization.authorization_id == binding.device_id
        || authorization.device_id != binding.device_id
        || authorization.authorization_generation != binding.authorization_generation
        || authorization.scope.organization_id != binding.scope.organization_id
        || authorization.scope.workspace_id != binding.scope.workspace_id
        || decode_sha256(&authorization.csr_sha256)? != binding.csr_sha256
        || decode_sha256(&authorization.csr_spki_sha256)? != binding.spki_sha256
        || response.codes.device_code_generation == 0
        || response.codes.device_code.len() != 64
        || !response
            .codes
            .device_code
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || response.codes.interval_seconds == 0
        || response.codes.interval_seconds > 60
        || response.authorization.expires_at != response.codes.expires_at
        || response.authorization.expires_at.trim().is_empty()
        || response.codes.verification_uri.trim().is_empty()
        || validate_user_code(&response.codes.user_code).is_err()
    {
        return Err(DeviceEnrollmentHttpError::Unavailable.into());
    }
    Ok(())
}

fn is_canonical_authorization_id(value: &str) -> bool {
    if value.len() != 22 {
        return false;
    }
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    bytes.len() == 16 && URL_SAFE_NO_PAD.encode(bytes) == value
}

fn validate_start_commit(
    committed: &DeviceEnrollmentAuthorizationSnapshot,
    binding: &DirectoryRegistrationBinding,
    response: &StartDeviceAuthorizationResponse,
) -> Result<(), HttpApiFailure> {
    let created = committed.start_disposition == Some(DeviceAuthorizationStartDisposition::Created);
    let recovered =
        committed.start_disposition == Some(DeviceAuthorizationStartDisposition::Recovered);
    if committed.authorization_id != response.authorization.authorization_id
        || committed.binding_id != binding.binding_id
        || committed.device_id != binding.device_id
        || committed.scope != binding.scope
        || committed.scope.organization_id != response.authorization.scope.organization_id
        || committed.scope.workspace_id != response.authorization.scope.workspace_id
        || committed.authorization_generation != binding.authorization_generation
        || committed.authorization_generation != response.authorization.authorization_generation
        || committed.csr_sha256 != binding.csr_sha256
        || committed.spki_sha256 != binding.spki_sha256
        || decode_sha256(&response.authorization.csr_sha256)? != committed.csr_sha256
        || decode_sha256(&response.authorization.csr_spki_sha256)? != committed.spki_sha256
        || committed.device_code_generation != response.codes.device_code_generation
        || !committed.matches_device_code(&response.codes.device_code)
        || (created && (committed.revision != 0 || committed.device_code_generation != 1))
        || (recovered && (committed.revision == 0 || committed.device_code_generation < 2))
        || (!created && !recovered)
        || committed.expires_at_unix_ms == 0
    {
        return Err(contract_violation());
    }
    Ok(())
}

fn validate_device_code(code: &str) -> Result<(), DeviceEnrollmentHttpError> {
    if code.len() != 64 || !code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DeviceEnrollmentHttpError::InvalidRequest);
    }
    Ok(())
}

fn validate_user_code(code: &str) -> Result<(), DeviceEnrollmentHttpError> {
    let normalized = code.to_ascii_uppercase();
    let parts = normalized.split('-').collect::<Vec<_>>();
    let valid = parts.len() == 3
        && parts.iter().all(|part| {
            part.len() == 4
                && part.bytes().all(|byte| {
                    byte.is_ascii_uppercase() && byte != b'I' && byte != b'O'
                        || byte.is_ascii_digit() && byte >= b'2'
                })
        });
    if !valid {
        return Err(DeviceEnrollmentHttpError::InvalidRequest);
    }
    Ok(())
}

struct AuthorizationWireSnapshot {
    authorization_id: String,
    device_id: String,
    scope: DeviceAuthorizationScope,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    authorization_generation: u64,
    expires_at: String,
}

fn response_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, HttpApiFailure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(contract_violation)
}

fn response_identifier(value: &Value, field: &str) -> Result<String, HttpApiFailure> {
    let identifier = response_string(value, field)?;
    if !(16..=128).contains(&identifier.len()) {
        return Err(contract_violation());
    }
    Ok(identifier.to_owned())
}

fn response_sha256(value: &Value, field: &str) -> Result<[u8; 32], HttpApiFailure> {
    decode_sha256(response_string(value, field)?).map_err(|_| contract_violation())
}

fn response_scope(value: &Value, field: &str) -> Result<DeviceAuthorizationScope, HttpApiFailure> {
    let scope = value.get(field).ok_or_else(contract_violation)?;
    let organization_id = response_string(scope, "organizationId")?;
    let workspace_id = response_string(scope, "workspaceId")?;
    if organization_id.len() > 256 || workspace_id.len() > 256 {
        return Err(contract_violation());
    }
    Ok(DeviceAuthorizationScope {
        organization_id: organization_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
    })
}

fn authorization_wire_snapshot(value: &Value) -> Result<AuthorizationWireSnapshot, HttpApiFailure> {
    let authorization_id = response_string(value, "authorizationId")?;
    if !is_canonical_authorization_id(authorization_id) {
        return Err(contract_violation());
    }
    let authorization_id = authorization_id.to_owned();
    let device_id = response_identifier(value, "deviceId")?;
    if authorization_id == device_id {
        return Err(contract_violation());
    }
    let scope = response_scope(value, "scope")?;
    let csr_sha256 = response_sha256(value, "csrSha256")?;
    let spki_sha256 = response_sha256(value, "csrSpkiSha256")?;
    let expires_at = response_string(value, "expiresAt")?.to_owned();
    let generation = value
        .get("authorizationGeneration")
        .and_then(Value::as_u64)
        .filter(|generation| *generation > 0)
        .ok_or_else(contract_violation)?;
    Ok(AuthorizationWireSnapshot {
        authorization_id,
        device_id,
        scope,
        csr_sha256,
        spki_sha256,
        authorization_generation: generation,
        expires_at,
    })
}

fn required_authorization_snapshot(
    value: &Value,
) -> Result<AuthorizationWireSnapshot, HttpApiFailure> {
    authorization_wire_snapshot(value.get("authorization").ok_or_else(contract_violation)?)
}

fn validate_approval_challenge_response(
    value: &Value,
    expected_scope: &DeviceAuthorizationScope,
) -> Result<(), HttpApiFailure> {
    if required_authorization_snapshot(value)?.scope != *expected_scope {
        return Err(contract_violation());
    }
    response_identifier(value, "approvalId")?;
    if !value.get("webauthnOptions").is_some_and(Value::is_object) {
        return Err(contract_violation());
    }
    response_string(value, "challengeExpiresAt")?;
    Ok(())
}

fn validate_user_identity(
    value: &Value,
    field: &str,
    expected: &UserIdentityRef,
) -> Result<(), HttpApiFailure> {
    let identity = value.get(field).ok_or_else(contract_violation)?;
    if response_string(identity, "issuer")? != expected.issuer
        || response_string(identity, "subject")? != expected.subject
    {
        return Err(contract_violation());
    }
    Ok(())
}

fn validate_complete_approval_response(
    value: &Value,
    expected_approver: &UserIdentityRef,
    accepted: bool,
) -> Result<(), HttpApiFailure> {
    required_authorization_snapshot(value)?;
    let state = response_string(value, "state")?;
    if !matches!(
        state,
        "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING"
            | "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERY_PENDING"
            | "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED"
    ) {
        return Err(contract_violation());
    }
    if accepted == (state == "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING") {
        return Err(contract_violation());
    }
    validate_user_identity(value, "approvedBy", expected_approver)?;
    response_string(value, "approvedAt")?;
    Ok(())
}

fn validate_denial_response(
    value: &Value,
    expected_approver: &UserIdentityRef,
    expected_scope: &DeviceAuthorizationScope,
) -> Result<(), HttpApiFailure> {
    let authorization = required_authorization_snapshot(value)?;
    if &authorization.scope != expected_scope {
        return Err(contract_violation());
    }
    validate_user_identity(value, "deniedBy", expected_approver)?;
    response_string(value, "deniedAt")?;
    Ok(())
}

fn validate_poll_response(
    response: &PollHttpResponse,
    device_code: &str,
) -> Result<(), HttpApiFailure> {
    let authorization = required_authorization_snapshot(&response.body)?;
    let committed = &response.committed_snapshot;
    if !committed.matches_device_code(device_code)
        || authorization.authorization_id != committed.authorization_id
        || authorization.device_id != committed.device_id
        || authorization.scope != committed.scope
        || authorization.csr_sha256 != committed.csr_sha256
        || authorization.spki_sha256 != committed.spki_sha256
        || authorization.authorization_generation != committed.authorization_generation
        || authorization.expires_at.trim().is_empty()
    {
        return Err(contract_violation());
    }
    let error = response
        .body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !poll_outcome_matches_snapshot(committed.poll_state, response.status, error) {
        return Err(contract_violation());
    }
    let status = response_string(&response.body, "status")?;
    match (response.status, error) {
        (PollHttpStatus::ProtocolError, "authorization_pending") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_PENDING"
                || !(1..=60).contains(
                    &response
                        .body
                        .get("intervalSeconds")
                        .and_then(Value::as_u64)
                        .ok_or_else(contract_violation)?,
                )
            {
                return Err(contract_violation());
            }
            response_string(&response.body, "nextPollAt")?;
            if response.retry_after_seconds.is_some() {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "slow_down") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_SLOW_DOWN"
                || !(1..=60).contains(
                    &response
                        .body
                        .get("intervalSeconds")
                        .and_then(Value::as_u64)
                        .ok_or_else(contract_violation)?,
                )
            {
                return Err(contract_violation());
            }
            response_string(&response.body, "nextPollAt")?;
            let retry_after = response
                .body
                .get("retryAfterSeconds")
                .and_then(Value::as_u64)
                .filter(|retry_after| *retry_after > 0)
                .ok_or_else(contract_violation)?;
            if response.retry_after_seconds != Some(retry_after) {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "access_denied") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_DENIED" {
                return Err(contract_violation());
            }
            response_string(&response.body, "decidedAt")?;
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "expired_token") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_EXPIRED" {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "invalid_grant") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_CONSUMED" {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "delivery_expired") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_EXPIRED"
                || !matches!(
                    response_string(&response.body, "recoveryStatus")?,
                    "DEVICE_DELIVERY_RECOVERY_STATUS_CERTIFICATE_REVOKED"
                        | "DEVICE_DELIVERY_RECOVERY_STATUS_CERTIFICATE_RETIRED"
                )
            {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::ProtocolError, "delivery_recovery_blocked") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_DELIVERY_RECOVERY_BLOCKED"
                || !matches!(
                    response_string(&response.body, "recoveryStatus")?,
                    "DEVICE_DELIVERY_RECOVERY_STATUS_REVOCATION_PENDING"
                        | "DEVICE_DELIVERY_RECOVERY_STATUS_RECOVERY_BLOCKED"
                )
            {
                return Err(contract_violation());
            }
            required_authorization_snapshot(&response.body)?;
        }
        (PollHttpStatus::Approved, "") => {
            if status != "DEVICE_AUTHORIZATION_POLL_STATUS_APPROVED" {
                return Err(contract_violation());
            }
            validate_approved_delivery(&response.body)?;
            if response.retry_after_seconds.is_some() {
                return Err(contract_violation());
            }
        }
        _ => return Err(contract_violation()),
    }
    Ok(())
}

fn enrollment_poll_state(state: &DeviceAuthorizationState) -> DeviceEnrollmentPollState {
    match state {
        DeviceAuthorizationState::Pending
        | DeviceAuthorizationState::AwaitingWebAuthn { .. }
        | DeviceAuthorizationState::VerifyingWebAuthn { .. }
        | DeviceAuthorizationState::Issuing { .. } => DeviceEnrollmentPollState::Pending,
        DeviceAuthorizationState::DeliveryPending { .. } => {
            DeviceEnrollmentPollState::DeliveryPending
        }
        DeviceAuthorizationState::Delivered { .. } => DeviceEnrollmentPollState::Delivered,
        DeviceAuthorizationState::RetirementPending { .. } => {
            DeviceEnrollmentPollState::RetirementPending
        }
        DeviceAuthorizationState::DeliveryExpired { .. } => {
            DeviceEnrollmentPollState::DeliveryExpired
        }
        DeviceAuthorizationState::Denied { .. } => DeviceEnrollmentPollState::Denied,
        DeviceAuthorizationState::Expired => DeviceEnrollmentPollState::Expired,
        DeviceAuthorizationState::IssuanceFailed { .. } => {
            DeviceEnrollmentPollState::IssuanceFailed
        }
        DeviceAuthorizationState::Consumed { .. } => DeviceEnrollmentPollState::Delivered,
        DeviceAuthorizationState::SupersededForRegistrationRotation { .. } => {
            DeviceEnrollmentPollState::Superseded
        }
        DeviceAuthorizationState::RegistrationRetired { .. } => {
            DeviceEnrollmentPollState::RegistrationRetired
        }
    }
}

fn poll_outcome_matches_snapshot(
    snapshot: DeviceEnrollmentPollState,
    status: PollHttpStatus,
    error: &str,
) -> bool {
    use DeviceEnrollmentPollState as State;

    matches!(
        (status, error, snapshot),
        (
            PollHttpStatus::ProtocolError,
            "authorization_pending",
            State::Pending
        ) | (PollHttpStatus::ProtocolError, "slow_down", State::Pending)
            | (
                PollHttpStatus::ProtocolError,
                "authorization_pending",
                State::DeliveryPending
            )
            | (
                PollHttpStatus::ProtocolError,
                "slow_down",
                State::DeliveryPending
            )
            | (
                PollHttpStatus::ProtocolError,
                "access_denied",
                State::Denied
            )
            | (
                PollHttpStatus::ProtocolError,
                "expired_token",
                State::Expired
            )
            | (
                PollHttpStatus::ProtocolError,
                "invalid_grant",
                State::Delivered
            )
            | (
                PollHttpStatus::ProtocolError,
                "delivery_expired",
                State::DeliveryExpired
            )
            | (
                PollHttpStatus::ProtocolError,
                "delivery_recovery_blocked",
                State::RetirementPending
            )
            | (PollHttpStatus::Approved, "", State::DeliveryPending)
    )
}

fn validate_approved_delivery(value: &Value) -> Result<(), HttpApiFailure> {
    let authorization = required_authorization_snapshot(value)?;
    let delivery = value.get("delivery").ok_or_else(contract_violation)?;
    response_identifier(delivery, "deliveryId")?;
    let certificate_der = STANDARD
        .decode(response_string(delivery, "certificateDer")?)
        .map_err(|_| contract_violation())?;
    if certificate_der.is_empty() || certificate_der.len() > MAX_CSR_DER_BYTES {
        return Err(contract_violation());
    }
    let certificate_sha256: [u8; 32] = Sha256::digest(&certificate_der).into();
    let chain = delivery
        .get("caChainDer")
        .and_then(Value::as_array)
        .ok_or_else(contract_violation)?;
    for item in chain {
        let der = STANDARD
            .decode(item.as_str().ok_or_else(contract_violation)?)
            .map_err(|_| contract_violation())?;
        if der.is_empty() || der.len() > MAX_CSR_DER_BYTES {
            return Err(contract_violation());
        }
    }
    let certificate = delivery.get("certificate").ok_or_else(contract_violation)?;
    if response_identifier(certificate, "deviceId")? != authorization.device_id
        || response_identifier(certificate, "authorizationId")? != authorization.authorization_id
        || response_scope(certificate, "scope")? != authorization.scope
        || response_sha256(certificate, "csrSha256")? != authorization.csr_sha256
        || response_sha256(certificate, "csrSpkiSha256")? != authorization.spki_sha256
        || response_sha256(certificate, "certificateSha256")? != certificate_sha256
    {
        return Err(contract_violation());
    }
    response_string(certificate, "serialNumber")?;
    response_string(certificate, "issuerId")?;
    response_string(certificate, "notBefore")?;
    response_string(certificate, "notAfter")?;
    response_string(certificate, "status")?;
    response_string(certificate, "purpose")?;
    response_string(delivery, "acknowledgementDeadline")?;
    Ok(())
}

fn validate_acknowledgement_response(
    value: &Value,
    expected_authorization_id: &str,
    expected_delivery_id: &str,
    expected_certificate_sha256: &[u8; 32],
) -> Result<(), HttpApiFailure> {
    let authorization = required_authorization_snapshot(value)?;
    if authorization.authorization_id != expected_authorization_id
        || response_identifier(value, "deliveryId")? != expected_delivery_id
        || response_sha256(value, "certificateSha256")? != *expected_certificate_sha256
    {
        return Err(contract_violation());
    }
    response_string(value, "acknowledgedAt")?;
    if response_string(value, "state")? != "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED" {
        return Err(contract_violation());
    }
    Ok(())
}

fn contract_violation() -> HttpApiFailure {
    DeviceEnrollmentHttpError::Unavailable.into()
}

fn ensure_no_secret_fields(value: &Value, allow_start_codes: bool) -> Result<(), HttpApiFailure> {
    fn walk(value: &Value, allow_start_codes: bool) -> bool {
        match value {
            Value::Object(object) => object.iter().any(|(key, value)| {
                let normalized = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                let forbidden = normalized.contains("registrationkey")
                    || normalized.contains("opaque")
                    || normalized.contains("webauthnassertion")
                    || normalized.contains("privatekey")
                    || normalized.contains("microsoft")
                    || normalized.contains("oauth")
                    || normalized.contains("accesstoken")
                    || normalized.contains("refreshtoken")
                    || normalized.contains("casigningkey")
                    || (!allow_start_codes
                        && (normalized == "devicecode" || normalized == "usercode"));
                forbidden || walk(value, allow_start_codes)
            }),
            Value::Array(items) => items.iter().any(|value| walk(value, allow_start_codes)),
            _ => false,
        }
    }

    if walk(value, allow_start_codes) {
        return Err(DeviceEnrollmentHttpError::Unavailable.into());
    }
    Ok(())
}

#[derive(Serialize)]
struct HttpApiErrorBody {
    error: &'static str,
}

struct HttpApiFailure(DeviceEnrollmentHttpError);

impl From<DeviceEnrollmentHttpError> for HttpApiFailure {
    fn from(error: DeviceEnrollmentHttpError) -> Self {
        Self(error)
    }
}

impl IntoResponse for HttpApiFailure {
    fn into_response(self) -> Response {
        let (status, code, retry_after) = match self.0 {
            DeviceEnrollmentHttpError::InvalidRequest => {
                (StatusCode::BAD_REQUEST, "invalid_request", None)
            }
            DeviceEnrollmentHttpError::Unauthorized => {
                (StatusCode::UNAUTHORIZED, "unauthorized", None)
            }
            DeviceEnrollmentHttpError::Forbidden => (StatusCode::FORBIDDEN, "access_denied", None),
            DeviceEnrollmentHttpError::InvalidGrant => {
                (StatusCode::BAD_REQUEST, "invalid_grant", None)
            }
            DeviceEnrollmentHttpError::Conflict => (StatusCode::CONFLICT, "state_conflict", None),
            DeviceEnrollmentHttpError::Expired => (StatusCode::GONE, "expired_token", None),
            DeviceEnrollmentHttpError::RateLimited {
                retry_after_seconds,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                "slow_down",
                Some(retry_after_seconds),
            ),
            DeviceEnrollmentHttpError::Unavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", None)
            }
        };
        let mut response = (status, Json(HttpApiErrorBody { error: code })).into_response();
        if let Some(seconds) = retry_after {
            let value = HeaderValue::from_str(&seconds.max(1).to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("1"));
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    #[derive(Default)]
    struct TestRegistration {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DeviceEnrollmentRegistrationTransactionPort for TestRegistration {
        async fn bind_and_start(
            &self,
            request: DeviceAuthorizationRegistrationRequest,
        ) -> Result<DeviceEnrollmentStartResult, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let binding = DirectoryRegistrationBinding {
                binding_id: [4; 16],
                device_id: "directory-device-0123456789".into(),
                scope: request.scope().clone(),
                authorization_generation: 1,
                csr_sha256: *request.csr_sha256(),
                spki_sha256: *request.spki_sha256(),
            };
            let authorization_id = URL_SAFE_NO_PAD.encode([0x42; 16]);
            let device_code = "a".repeat(64);
            let response = StartDeviceAuthorizationResponse {
                authorization: DeviceAuthorizationReferenceWire {
                    authorization_id: authorization_id.clone(),
                    device_id: binding.device_id.clone(),
                    scope: DeviceScopeWire {
                        organization_id: binding.scope.organization_id.clone(),
                        workspace_id: binding.scope.workspace_id.clone(),
                    },
                    csr_spki_sha256: STANDARD.encode(binding.spki_sha256),
                    csr_sha256: STANDARD.encode(binding.csr_sha256),
                    expires_at: "2026-09-26T12:10:00Z".into(),
                    authorization_generation: binding.authorization_generation,
                },
                codes: DeviceAuthorizationCodesWire {
                    device_code: device_code.clone(),
                    user_code: "ABCD-EFGH-JKLM".into(),
                    verification_uri: "https://example.invalid/verify".into(),
                    verification_uri_complete: None,
                    interval_seconds: 5,
                    expires_at: "2026-09-26T12:10:00Z".into(),
                    device_code_generation: 1,
                },
            };
            let committed_snapshot = DeviceEnrollmentAuthorizationSnapshot::test_fixture(
                DeviceEnrollmentAuthorizationSnapshotFixture {
                    authorization_id,
                    binding_id: binding.binding_id,
                    device_id: binding.device_id.clone(),
                    scope: binding.scope.clone(),
                    authorization_generation: binding.authorization_generation,
                    device_code_generation: 1,
                    csr_sha256: binding.csr_sha256,
                    spki_sha256: binding.spki_sha256,
                    revision: 0,
                    expires_at_unix_ms: 1,
                    device_code: &device_code,
                    poll_state: DeviceEnrollmentPollState::Pending,
                },
            );
            Ok(DeviceEnrollmentStartResult {
                binding,
                response,
                committed_snapshot,
            })
        }
    }

    struct TestCsrValidator([u8; 32]);

    impl DeviceCsrValidator for TestCsrValidator {
        fn validate_and_hash_spki(
            &self,
            _csr_der: &[u8],
        ) -> Result<[u8; 32], DeviceAuthorizationPortError> {
            Ok(self.0)
        }
    }

    struct TestAuthorization {
        calls: AtomicUsize,
        poll_retry_after: Option<u64>,
    }

    fn authorization_ref_json() -> Value {
        json!({
            "authorizationId":URL_SAFE_NO_PAD.encode([0x42; 16]),
            "deviceId":"directory-device-0123456789",
            "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
            "csrSpkiSha256":STANDARD.encode([1u8; 32]),
            "csrSha256":STANDARD.encode([3u8; 32]),
            "expiresAt":"2026-09-26T12:10:00Z",
            "authorizationGeneration":1,
        })
    }

    fn approved_poll_fixture() -> PollHttpResponse {
        let certificate_der = vec![0x30, 0x03, 0x02, 0x01, 0x01];
        let certificate_sha256: [u8; 32] = Sha256::digest(&certificate_der).into();
        PollHttpResponse {
            body: json!({
                "status":"DEVICE_AUTHORIZATION_POLL_STATUS_APPROVED",
                "authorization":authorization_ref_json(),
                "delivery":{
                    "deliveryId":"delivery-0123456789",
                    "certificateDer":STANDARD.encode(&certificate_der),
                    "caChainDer":[],
                    "certificate":{
                        "deviceId":"directory-device-0123456789",
                        "authorizationId":URL_SAFE_NO_PAD.encode([0x42; 16]),
                        "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
                        "serialNumber":"01",
                        "certificateSha256":STANDARD.encode(certificate_sha256),
                        "csrSpkiSha256":STANDARD.encode([1u8; 32]),
                        "csrSha256":STANDARD.encode([3u8; 32]),
                        "issuerId":"issuer-1",
                        "notBefore":"2026-09-26T12:00:00Z",
                        "notAfter":"2027-09-26T12:00:00Z",
                        "status":"WORKSPACE_DEVICE_CERTIFICATE_STATUS_DELIVERY_PENDING",
                        "purpose":"WORKSPACE_DEVICE_CERTIFICATE_PURPOSE_CONTROL_CLIENT_AUTH"
                    },
                    "acknowledgementDeadline":"2026-09-26T12:05:00Z"
                }
            }),
            status: PollHttpStatus::Approved,
            retry_after_seconds: None,
            committed_snapshot: DeviceEnrollmentAuthorizationSnapshot::test_fixture(
                DeviceEnrollmentAuthorizationSnapshotFixture {
                    authorization_id: URL_SAFE_NO_PAD.encode([0x42; 16]),
                    binding_id: [4; 16],
                    device_id: "directory-device-0123456789".into(),
                    scope: DeviceAuthorizationScope {
                        organization_id: "org-1".into(),
                        workspace_id: "workspace-1".into(),
                    },
                    authorization_generation: 1,
                    device_code_generation: 1,
                    csr_sha256: [3; 32],
                    spki_sha256: [1; 32],
                    revision: 1,
                    expires_at_unix_ms: 1,
                    device_code: &"a".repeat(64),
                    poll_state: DeviceEnrollmentPollState::DeliveryPending,
                },
            ),
        }
    }

    #[async_trait]
    impl DeviceEnrollmentAuthorizationPort for TestAuthorization {
        async fn begin_approval(
            &self,
            _user: &UserIdentityRef,
            _abuse_key: &[u8; 32],
            _user_code: &str,
            _scope: DeviceAuthorizationScope,
        ) -> Result<Value, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({
                "authorization":authorization_ref_json(),
                "approvalId": "approval-0123456789",
                "webauthnOptions": {"challenge":"public-options"},
                "challengeExpiresAt":"2026-09-26T12:01:00Z",
            }))
        }

        async fn complete_approval(
            &self,
            _user: &UserIdentityRef,
            _approval_id: String,
            _assertion_json: Option<SecretBytes>,
        ) -> Result<CompleteApprovalHttpResponse, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompleteApprovalHttpResponse {
                body: json!({
                    "authorization":authorization_ref_json(),
                    "state":"DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING",
                    "approvedBy":{"issuer":"https://issuer.invalid","subject":"alice"},
                    "approvedAt":"2026-09-26T12:01:00Z",
                }),
                accepted: false,
            })
        }

        async fn deny(
            &self,
            _user: &UserIdentityRef,
            _abuse_key: &[u8; 32],
            _user_code: &str,
            _scope: DeviceAuthorizationScope,
        ) -> Result<Value, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(DeviceEnrollmentHttpError::Conflict)
        }

        async fn poll(
            &self,
            device_code: &str,
        ) -> Result<PollHttpResponse, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let error = if self.poll_retry_after.is_some() {
                "slow_down"
            } else {
                "authorization_pending"
            };
            let body = if let Some(retry_after) = self.poll_retry_after {
                json!({
                    "status":"DEVICE_AUTHORIZATION_POLL_STATUS_SLOW_DOWN",
                    "error":error,
                    "authorization":authorization_ref_json(),
                    "intervalSeconds":10,
                    "nextPollAt":"2026-09-26T12:01:10Z",
                    "retryAfterSeconds":retry_after,
                })
            } else {
                json!({
                    "status":"DEVICE_AUTHORIZATION_POLL_STATUS_PENDING",
                    "error":error,
                    "authorization":authorization_ref_json(),
                    "intervalSeconds":5,
                    "nextPollAt":"2026-09-26T12:01:05Z",
                })
            };
            Ok(PollHttpResponse {
                body,
                status: PollHttpStatus::ProtocolError,
                retry_after_seconds: self.poll_retry_after,
                committed_snapshot: DeviceEnrollmentAuthorizationSnapshot::test_fixture(
                    DeviceEnrollmentAuthorizationSnapshotFixture {
                        authorization_id: URL_SAFE_NO_PAD.encode([0x42; 16]),
                        binding_id: [4; 16],
                        device_id: "directory-device-0123456789".into(),
                        scope: DeviceAuthorizationScope {
                            organization_id: "org-1".into(),
                            workspace_id: "workspace-1".into(),
                        },
                        authorization_generation: 1,
                        device_code_generation: 1,
                        csr_sha256: [3; 32],
                        spki_sha256: [1; 32],
                        revision: 1,
                        expires_at_unix_ms: 1,
                        device_code,
                        poll_state: DeviceEnrollmentPollState::Pending,
                    },
                ),
            })
        }

        async fn acknowledge_delivery(
            &self,
            _request: AcknowledgeDeviceDeliveryCommand,
        ) -> Result<Value, DeviceEnrollmentHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(DeviceEnrollmentHttpError::Conflict)
        }
    }

    fn dependencies(
        authorization: Option<Arc<dyn DeviceEnrollmentAuthorizationPort>>,
    ) -> DeviceEnrollmentHttpDependencies {
        DeviceEnrollmentHttpDependencies {
            registration: Some(Arc::new(TestRegistration::default())),
            csr_validator: Some(Arc::new(TestCsrValidator([1; 32]))),
            authorization,
        }
    }

    async fn send(
        router: Router,
        method: Method,
        uri: &str,
        body: Value,
        user: Option<TrustedInteractiveUserSession>,
        abuse_key: Option<TrustedEnrollmentAbuseKey>,
    ) -> (StatusCode, String, Option<String>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(user) = user {
            builder = builder.extension(user);
        }
        if let Some(abuse_key) = abuse_key {
            builder = builder.extension(abuse_key);
        }
        let response = router
            .oneshot(
                builder
                    .body(Body::from(body.to_string()))
                    .expect("valid HTTP request"),
            )
            .await
            .expect("router response");
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|value| value.to_str().expect("valid retry-after header").to_owned());
        let bytes = to_bytes(response.into_body(), MAX_HTTP_BODY_BYTES)
            .await
            .expect("bounded response body");
        (
            status,
            String::from_utf8(bytes.to_vec()).expect("JSON UTF-8"),
            retry_after,
        )
    }

    fn trusted_user() -> TrustedInteractiveUserSession {
        TrustedInteractiveUserSession::from_verified_identity(
            "https://issuer.invalid".into(),
            "alice".into(),
        )
        .expect("verified identity")
    }

    fn trusted_abuse_key() -> TrustedEnrollmentAbuseKey {
        TrustedEnrollmentAbuseKey::from_server_digest([8; 32])
    }

    fn start_request(registration_key: &str, spki_digest: [u8; 32]) -> Value {
        json!({
            "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
            "csrDer":STANDARD.encode([3u8; 64]),
            "csrSpkiSha256":STANDARD.encode(spki_digest),
            "registrationKey":registration_key,
        })
    }

    #[tokio::test]
    async fn start_returns_directory_identity_and_keeps_recovery_key_private() {
        let registration = Arc::new(TestRegistration::default());
        let router = device_enrollment_v1_router(DeviceEnrollmentHttpDependencies {
            registration: Some(registration.clone()),
            csr_validator: Some(Arc::new(TestCsrValidator([1; 32]))),
            authorization: None,
        });
        let recovery_key = STANDARD.encode([7u8; 32]);
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations",
            start_request(&recovery_key, [1; 32]),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let response: Value = serde_json::from_str(&body).expect("JSON response");
        let authorization_ref = &response["authorization"];
        assert_ne!(
            authorization_ref["authorizationId"],
            authorization_ref["deviceId"]
        );
        assert_eq!(authorization_ref["deviceId"], "directory-device-0123456789");
        assert_eq!(authorization_ref["authorizationGeneration"], 1);
        assert_eq!(response["codes"]["deviceCodeGeneration"], 1);
        assert!(!body.contains(&recovery_key));
        assert_eq!(registration.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_atomic_registration_transaction_fails_closed() {
        let router = device_enrollment_v1_router(DeviceEnrollmentHttpDependencies {
            registration: None,
            csr_validator: Some(Arc::new(TestCsrValidator([1; 32]))),
            authorization: None,
        });
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations",
            start_request(&STANDARD.encode([7u8; 32]), [1; 32]),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }

    #[tokio::test]
    async fn mismatched_csr_digest_is_rejected_before_registration_transaction() {
        let registration = Arc::new(TestRegistration::default());
        let router = device_enrollment_v1_router(DeviceEnrollmentHttpDependencies {
            registration: Some(registration.clone()),
            csr_validator: Some(Arc::new(TestCsrValidator([1; 32]))),
            authorization: None,
        });
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations",
            start_request(&STANDARD.encode([7u8; 32]), [2; 32]),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(registration.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn approval_requires_verified_session_and_server_abuse_key() {
        let authorization = Arc::new(TestAuthorization {
            calls: AtomicUsize::new(0),
            poll_retry_after: None,
        });
        let router = device_enrollment_v1_router(dependencies(Some(authorization.clone())));
        let request = json!({
            "userCode":"ABCD-EFGH-JKLM",
            "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
        });
        let (status, body, _) = send(
            router.clone(),
            Method::POST,
            "/v1/device-authorizations/approval-challenges",
            request.clone(),
            None,
            Some(trusted_abuse_key()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(authorization.calls.load(Ordering::SeqCst), 0);

        let forged = json!({
            "userCode":"ABCD-EFGH-JKLM",
            "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
            "approver":{"issuer":"forged","subject":"admin"},
        });
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations/approval-challenges",
            forged,
            Some(trusted_user()),
            Some(trusted_abuse_key()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(authorization.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn trusted_member_gets_public_webauthn_options_and_issuing_is_accepted() {
        let authorization = Arc::new(TestAuthorization {
            calls: AtomicUsize::new(0),
            poll_retry_after: None,
        });
        let router = device_enrollment_v1_router(dependencies(Some(authorization.clone())));
        let (status, challenge_body, _) = send(
            router.clone(),
            Method::POST,
            "/v1/device-authorizations/approval-challenges",
            json!({
                "userCode":"ABCD-EFGH-JKLM",
                "scope":{"organizationId":"org-1","workspaceId":"workspace-1"},
            }),
            Some(trusted_user()),
            Some(trusted_abuse_key()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{challenge_body}");
        assert!(challenge_body.contains("public-options"));
        assert!(!challenge_body.contains("opaque"));

        let (status, complete_body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations/approval-challenges/approval-0123456789/complete",
            json!({"webauthnAssertion":{"id":"credential","response":{"signature":"assertion"}}}),
            Some(trusted_user()),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{complete_body}");
        assert!(complete_body.contains("ISSUING"));
        assert!(!complete_body.contains("assertion"));
        assert_eq!(authorization.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn poll_uses_device_code_and_maps_pending_to_rfc_protocol_status() {
        let authorization = Arc::new(TestAuthorization {
            calls: AtomicUsize::new(0),
            poll_retry_after: None,
        });
        let router = device_enrollment_v1_router(dependencies(Some(authorization.clone())));
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations/poll",
            json!({"deviceCode":"a".repeat(64)}),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("authorization_pending"));
        assert_eq!(authorization.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poll_slow_down_sets_contract_retry_after_header() {
        let authorization = Arc::new(TestAuthorization {
            calls: AtomicUsize::new(0),
            poll_retry_after: Some(7),
        });
        let router = device_enrollment_v1_router(dependencies(Some(authorization)));
        let (status, body, retry_after) = send(
            router,
            Method::POST,
            "/v1/device-authorizations/poll",
            json!({"deviceCode":"a".repeat(64)}),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("slow_down"));
        assert_eq!(retry_after.as_deref(), Some("7"));
    }

    #[test]
    fn approved_poll_binds_directory_identity_and_certificate_to_csr() {
        let mut response = approved_poll_fixture();
        assert!(validate_poll_response(&response, &"a".repeat(64)).is_ok());

        response.body["delivery"]["certificate"]["csrSha256"] =
            Value::String(STANDARD.encode([9u8; 32]));
        assert!(validate_poll_response(&response, &"a".repeat(64)).is_err());
    }

    #[test]
    fn acknowledgement_receipt_must_match_exact_request_tuple() {
        let body = json!({
            "authorization":authorization_ref_json(),
            "deliveryId":"delivery-0123456789",
            "certificateSha256":STANDARD.encode([4u8; 32]),
            "acknowledgedAt":"2026-09-26T12:01:00Z",
            "state":"DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED"
        });
        assert!(validate_acknowledgement_response(
            &body,
            &URL_SAFE_NO_PAD.encode([0x42; 16]),
            "delivery-0123456789",
            &[4; 32]
        )
        .is_ok());
        assert!(validate_acknowledgement_response(
            &body,
            &URL_SAFE_NO_PAD.encode([0x42; 16]),
            "other-delivery-0123456789",
            &[4; 32]
        )
        .is_err());
    }

    #[test]
    fn response_guard_rejects_secret_fields_across_wire_naming_styles() {
        for field in [
            "device_code",
            "registration-key",
            "opaque_state",
            "webauthnAssertion",
            "ca_private_key",
            "microsoft_access_token",
            "oauthRefreshToken",
        ] {
            assert!(
                ensure_no_secret_fields(&json!({field:"secret"}), false).is_err(),
                "secret field {field} must not be serialized"
            );
        }
    }

    #[tokio::test]
    async fn acknowledgement_conflict_is_never_reported_as_delivery_success() {
        let authorization = Arc::new(TestAuthorization {
            calls: AtomicUsize::new(0),
            poll_retry_after: None,
        });
        let router = device_enrollment_v1_router(dependencies(Some(authorization.clone())));
        let (status, body, _) = send(
            router,
            Method::POST,
            "/v1/device-authorizations/delivery-acknowledgements",
            json!({
                "authorizationId":URL_SAFE_NO_PAD.encode([0x42; 16]),
                "deviceCode":"a".repeat(64),
                "deliveryId":"delivery-0123456789",
                "certificateSha256":STANDARD.encode([1u8; 32]),
                "csrSpkiSha256":STANDARD.encode([2u8; 32]),
                "csrSha256":STANDARD.encode([3u8; 32]),
            }),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("state_conflict"));
        assert_eq!(authorization.calls.load(Ordering::SeqCst), 1);
    }
}
