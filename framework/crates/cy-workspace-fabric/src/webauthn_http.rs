//! Fail-closed HTTP composition for SSO-bound WebAuthn credential enrollment.
//!
//! The BFF creates [`VerifiedWebSessionContext`] only after it verifies the
//! current Azure AD bearer and its session-bound CSRF token. Browser fields
//! never choose the credential owner or organization. Production mounting
//! requires PostgreSQL credentials, Directory authorization, and a durable
//! [`WebAuthnHttpSessionBindingStore`]; there is no in-memory production path.
//!
//! SSO 绑定的 WebAuthn 凭据登记 HTTP 组合。BFF 验证 Azure AD bearer 与 session CSRF 后创建可信上下文；
//! 浏览器字段不能指定 credential owner 或 organization。生产挂载必须接入 PostgreSQL 凭据、Directory
//! 授权及持久化 session binding store；本模块没有生产内存后备实现。

use std::{fmt, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::task;
use tonic::async_trait;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

use cy_proto::workspace_v1::UserIdentityRef;

use crate::{
    caller::WORKSPACE_MEMBER_ROLE,
    device_authorization::DeviceAuthorizationPortError,
    device_authorization::{DeviceAuthorizationClockPort, DeviceAuthorizationId},
    directory::{WorkspaceDirectory, WorkspaceDirectoryError},
    web_identity::VerifiedWebPrincipal,
    webauthn_credential_store::{WebAuthnCredentialStore, WebAuthnCredentialStoreError},
    webauthn_postgres_store::PostgresWebAuthnCredentialStore,
    webauthn_verifier::{
        WebAuthnCredentialEnrollmentAuthorizer, WebAuthnCredentialEnrollmentService,
        WebAuthnCredentialRegistrationChallenge, WebAuthnVerifierConfig,
        WebAuthnVerifierConfigError,
    },
};

const MAX_HTTP_BODY_BYTES: usize = 128 * 1024;
const MAX_REGISTRATION_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_WORKSPACE_ID_BYTES: usize = 128;
const CSRF_HEADER_NAME: &str = "x-csrf-token";
const CSRF_COOKIE_NAME: &str = "__Secure-cyrene-csrf";
const SESSION_BINDING_DOMAIN: &[u8] = b"cyrene.workspace.webauthn.session-binding.v1\0";

type HmacSha256 = Hmac<Sha256>;

/// Opaque digest binding an HTTP ceremony to one exact BFF-authenticated bearer session.
///
/// The digest is safe to persist in the server-side ceremony binding table. It is
/// not a bearer token and must still be treated as private correlation material.
#[derive(Clone, PartialEq, Eq)]
pub struct WebAuthnSessionBindingDigest([u8; 32]);

impl WebAuthnSessionBindingDigest {
    /// Returns the opaque digest bytes for a trusted server-side persistence adapter.
    ///
    /// Never put this value in browser JSON, URLs, logs, or telemetry.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compare two digests without data-dependent early exit.
    pub fn constant_time_eq(&self, other: &Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

impl fmt::Debug for WebAuthnSessionBindingDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("WebAuthnSessionBindingDigest")
            .field(&"[REDACTED]")
            .finish()
    }
}

/// BFF-authenticated principal and bearer-bound session evidence for one request.
///
/// A BFF must create this value only after verifying the same bearer with
/// [`crate::WebPrincipalVerifier`] and successfully checking its CSRF token.
/// Refreshing the bearer changes `session_binding`, so existing ceremonies must
/// be restarted. This type is deliberately not serializable or clone-debuggable.
#[derive(Clone)]
pub struct VerifiedWebSessionContext {
    principal: VerifiedWebPrincipal,
    session_binding: WebAuthnSessionBindingDigest,
    csrf_token_sha256: [u8; 32],
}

impl VerifiedWebSessionContext {
    /// Create a context inside the trusted BFF after access-token and CSRF verification.
    ///
    /// `access_token` must be the exact string that produced `principal`; it is
    /// hashed before MAC input and is never retained. `csrf_mac_key` must be the
    /// BFF's server-only CSRF MAC key. The supplied CSRF token must already have
    /// been issued for and validated against this principal and bearer.
    pub fn from_bff_verified_access_token(
        principal: VerifiedWebPrincipal,
        access_token: &str,
        csrf_mac_key: &[u8; 32],
        issued_csrf_token: &str,
    ) -> Result<Self, WebAuthnSessionContextError> {
        if csrf_mac_key.iter().all(|byte| *byte == 0)
            || access_token.is_empty()
            || access_token.len() > 16 * 1024
            || issued_csrf_token.is_empty()
            || issued_csrf_token.len() > 256
            || principal.identity().issuer.trim().is_empty()
            || principal.identity().subject.trim().is_empty()
            || principal.organization_id().trim().is_empty()
            || principal.expires_at_unix_ms() <= 0
        {
            return Err(WebAuthnSessionContextError::InvalidInput);
        }

        let access_token_sha256: [u8; 32] = Sha256::digest(access_token.as_bytes()).into();
        let mut mac = HmacSha256::new_from_slice(csrf_mac_key)
            .map_err(|_| WebAuthnSessionContextError::InvalidInput)?;
        mac.update(SESSION_BINDING_DOMAIN);
        update_hmac_field(&mut mac, &access_token_sha256);
        update_hmac_field(&mut mac, principal.identity().issuer.as_bytes());
        update_hmac_field(&mut mac, principal.identity().subject.as_bytes());
        update_hmac_field(&mut mac, principal.organization_id().as_bytes());
        update_hmac_field(&mut mac, &principal.expires_at_unix_ms().to_be_bytes());
        let mut session_binding = [0_u8; 32];
        session_binding.copy_from_slice(&mac.finalize().into_bytes());

        let csrf_token_sha256 = Sha256::digest(issued_csrf_token.as_bytes()).into();

        Ok(Self {
            principal,
            session_binding: WebAuthnSessionBindingDigest(session_binding),
            csrf_token_sha256,
        })
    }

    /// Returns the principal whose token and organization were verified by the BFF.
    pub fn principal(&self) -> &VerifiedWebPrincipal {
        &self.principal
    }

    /// Returns the opaque bearer-session digest for server-side store adapters.
    pub fn session_binding(&self) -> &WebAuthnSessionBindingDigest {
        &self.session_binding
    }

    /// Returns whether the verified access-token principal remains unexpired.
    pub fn is_valid_at(&self, now_unix_ms: u64) -> bool {
        i64::try_from(now_unix_ms)
            .map(|now| self.principal.expires_at_unix_ms() > now)
            .unwrap_or(false)
    }

    fn verify_csrf_headers(&self, headers: &HeaderMap) -> Result<(), WebAuthnHttpError> {
        let token_header =
            exactly_one_header(headers, CSRF_HEADER_NAME).ok_or(WebAuthnHttpError::Forbidden)?;
        let token_cookie = csrf_cookie(headers).ok_or(WebAuthnHttpError::Forbidden)?;
        if token_header.is_empty()
            || token_header.len() > 256
            || !bool::from(token_header.ct_eq(token_cookie.as_bytes()))
        {
            return Err(WebAuthnHttpError::Forbidden);
        }
        let presented_sha256: [u8; 32] = Sha256::digest(token_header).into();
        if !bool::from(presented_sha256.ct_eq(&self.csrf_token_sha256)) {
            return Err(WebAuthnHttpError::Forbidden);
        }
        Ok(())
    }
}

impl fmt::Debug for VerifiedWebSessionContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedWebSessionContext")
            .field("principal", &"[VERIFIED]")
            .field("session_binding", &self.session_binding)
            .field("csrf_token", &"[REDACTED]")
            .finish()
    }
}

/// Invalid BFF session-context input.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnSessionContextError {
    /// A required trusted value is absent or outside the supported size bounds.
    #[error("invalid verified web session context")]
    InvalidInput,
}

/// One server-side ceremony namespace used to prevent cross-flow ID confusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WebAuthnHttpCeremonyPurpose {
    /// SSO user's controlled credential-registration ceremony.
    CredentialRegistration,
    /// Device-authorization approval ceremony.
    DeviceApproval,
}

/// Durable owner, scope, expiry, and bearer-session binding for one ceremony.
///
/// A storage adapter must persist the digest, owner and scope in the same row,
/// audit state transitions, and enforce compare-and-swap semantics for finish.
#[derive(Clone)]
pub struct WebAuthnSessionCeremonyBinding {
    purpose: WebAuthnHttpCeremonyPurpose,
    ceremony_id: DeviceAuthorizationId,
    owner: UserIdentityRef,
    organization_id: String,
    workspace_id: String,
    session_binding: WebAuthnSessionBindingDigest,
    expires_at_unix_ms: u64,
}

impl WebAuthnSessionCeremonyBinding {
    /// Build a binding from server-derived identity and scope for a known flow.
    pub fn from_verified_session(
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        workspace_id: impl Into<String>,
        expires_at_unix_ms: u64,
    ) -> Result<Self, WebAuthnHttpSessionBindingError> {
        let workspace_id = workspace_id.into();
        if workspace_id.trim().is_empty()
            || workspace_id.len() > MAX_WORKSPACE_ID_BYTES
            || workspace_id.chars().any(char::is_control)
            || expires_at_unix_ms == 0
            || u64::try_from(session.principal.expires_at_unix_ms())
                .map(|session_expiry| expires_at_unix_ms > session_expiry)
                .unwrap_or(true)
        {
            return Err(WebAuthnHttpSessionBindingError::Conflict);
        }
        Ok(Self {
            purpose,
            ceremony_id,
            owner: session.principal.identity().clone(),
            organization_id: session.principal.organization_id().to_string(),
            workspace_id,
            session_binding: session.session_binding.clone(),
            expires_at_unix_ms,
        })
    }

    /// Returns the ceremony namespace.
    pub const fn purpose(&self) -> WebAuthnHttpCeremonyPurpose {
        self.purpose
    }

    /// Returns the server-generated ceremony ID.
    pub const fn ceremony_id(&self) -> &DeviceAuthorizationId {
        &self.ceremony_id
    }

    /// Returns the Directory-bound owner identity.
    pub fn owner(&self) -> &UserIdentityRef {
        &self.owner
    }

    /// Returns the organization resolved by the verified principal.
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the Directory-checked workspace scope.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the opaque current-session digest.
    pub fn session_binding(&self) -> &WebAuthnSessionBindingDigest {
        &self.session_binding
    }

    /// Returns the server-side ceremony expiry in Unix milliseconds.
    pub const fn expires_at_unix_ms(&self) -> u64 {
        self.expires_at_unix_ms
    }
}

impl fmt::Debug for WebAuthnSessionCeremonyBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebAuthnSessionCeremonyBinding")
            .field("purpose", &self.purpose)
            .field("ceremony_id", &"[REDACTED]")
            .field("owner", &"[REDACTED]")
            .field("organization_id", &"[REDACTED]")
            .field("workspace_id", &"[REDACTED]")
            .field("session_binding", &self.session_binding)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .finish()
    }
}

/// Result of reserving an assertion/registration finish under a session binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnSessionFinishReservation {
    /// This exact response digest may be verified or safely retried.
    Continue,
    /// The same response was already committed successfully.
    AlreadyComplete,
}

/// Errors from the durable session-to-ceremony authority.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnHttpSessionBindingError {
    /// No active ceremony exists, it expired, or a different session owns it.
    #[error("WebAuthn ceremony is unavailable")]
    NotFoundOrExpired,
    /// A different response digest already owns the finish reservation.
    #[error("WebAuthn ceremony finish conflicts with another response")]
    Conflict,
    /// Durable session-binding storage is unavailable or corrupt.
    #[error("WebAuthn session-binding storage is unavailable")]
    Unavailable,
}

/// Persistent session binding store shared by registration and approval flows.
///
/// Implementations must use a durable multi-replica database in production,
/// compare session digests in constant time, enforce expiry on every operation,
/// and atomically reserve/complete one response digest. In-memory storage is
/// suitable only for unit tests. Missing or failed operations fail closed as
/// HTTP 503.
#[async_trait]
pub trait WebAuthnHttpSessionBindingStore: Send + Sync {
    /// Persist one new server-generated ceremony ID and exact verified session.
    async fn bind_ceremony(
        &self,
        binding: WebAuthnSessionCeremonyBinding,
    ) -> Result<(), WebAuthnHttpSessionBindingError>;

    /// Load and authorize an active ceremony for the exact current session.
    async fn active_binding(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        now_unix_ms: u64,
    ) -> Result<WebAuthnSessionCeremonyBinding, WebAuthnHttpSessionBindingError>;

    /// Atomically reserve one response digest, idempotently for the same digest.
    async fn reserve_finish(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        response_sha256: [u8; 32],
        now_unix_ms: u64,
    ) -> Result<WebAuthnSessionFinishReservation, WebAuthnHttpSessionBindingError>;

    /// Mark a previously reserved response digest as successfully committed.
    async fn complete_finish(
        &self,
        purpose: WebAuthnHttpCeremonyPurpose,
        ceremony_id: &DeviceAuthorizationId,
        session: &VerifiedWebSessionContext,
        response_sha256: [u8; 32],
        now_unix_ms: u64,
    ) -> Result<(), WebAuthnHttpSessionBindingError>;
}

/// Directory-role failure when authorizing a verified web session.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnHttpAuthorizationError {
    /// The user is not a member or does not hold the required Directory role.
    #[error("Workspace membership or role is required")]
    Forbidden,
    /// The authoritative Directory could not answer the lookup.
    #[error("Workspace Directory is unavailable")]
    Unavailable,
}

/// Require the configured Directory role for the verified user and organization.
///
/// `workspace_id` is a requested resource selector, not identity or authority;
/// the Directory must confirm membership and role for that exact scope.
pub async fn authorize_verified_web_session_role(
    directory: &dyn WorkspaceDirectory,
    session: &VerifiedWebSessionContext,
    workspace_id: &str,
    required_role: &str,
) -> Result<(), WebAuthnHttpAuthorizationError> {
    if workspace_id.trim().is_empty()
        || workspace_id.len() > MAX_WORKSPACE_ID_BYTES
        || required_role.trim().is_empty()
    {
        return Err(WebAuthnHttpAuthorizationError::Forbidden);
    }
    let roles = directory
        .roles_for_member(
            session.principal.identity(),
            session.principal.organization_id(),
            workspace_id,
        )
        .await
        .map_err(map_directory_error)?;
    let Some(roles) = roles else {
        return Err(WebAuthnHttpAuthorizationError::Forbidden);
    };
    if required_role == WORKSPACE_MEMBER_ROLE || roles.contains(required_role) {
        Ok(())
    } else {
        Err(WebAuthnHttpAuthorizationError::Forbidden)
    }
}

fn map_directory_error(_: WorkspaceDirectoryError) -> WebAuthnHttpAuthorizationError {
    WebAuthnHttpAuthorizationError::Unavailable
}

/// Configuration failure while constructing the protected router.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnHttpConfigurationError {
    /// RP origin is not a single exact HTTPS origin, or role policy is invalid.
    #[error("WebAuthn HTTP security configuration is invalid")]
    InvalidConfiguration,
    /// The fixed verifier configuration could not be built.
    #[error("WebAuthn verifier configuration is invalid")]
    Verifier(#[from] WebAuthnVerifierConfigError),
}

/// Fail-closed HTTP state for the WebAuthn registration routes.
pub struct WebAuthnHttpState {
    rp_origin: Url,
    required_role: String,
    enrollment: Option<Arc<dyn WebAuthnCredentialEnrollmentHttpPort>>,
    directory: Option<Arc<dyn WorkspaceDirectory>>,
    session_bindings: Option<Arc<dyn WebAuthnHttpSessionBindingStore>>,
    clock: Option<Arc<dyn DeviceAuthorizationClockPort>>,
}

impl WebAuthnHttpState {
    /// Create an unconfigured route state whose protected operations return 503.
    ///
    /// This is useful while a deployment is deliberately blocked on production
    /// credential/session stores; it never substitutes a fake verifier.
    pub fn unavailable(
        rp_origin: Url,
        required_role: impl Into<String>,
    ) -> Result<Self, WebAuthnHttpConfigurationError> {
        let required_role = required_role.into();
        validate_fixed_origin(&rp_origin)?;
        validate_role(&required_role)?;
        Ok(Self {
            rp_origin,
            required_role,
            enrollment: None,
            directory: None,
            session_bindings: None,
            clock: None,
        })
    }

    /// Build production routes with the fixed-RP verifier and PostgreSQL storage.
    ///
    /// The additional session-binding store must be durable and shared across
    /// replicas. The caller must leave the router unmounted or use
    /// [`Self::unavailable`] until it has a production implementation.
    #[allow(clippy::too_many_arguments)]
    pub fn production(
        config: WebAuthnVerifierConfig,
        credential_store: Arc<PostgresWebAuthnCredentialStore>,
        directory: Arc<dyn WorkspaceDirectory>,
        clock: Arc<dyn DeviceAuthorizationClockPort>,
        authorizer: Arc<dyn WebAuthnCredentialEnrollmentAuthorizer>,
        session_bindings: Arc<dyn WebAuthnHttpSessionBindingStore>,
        required_role: impl Into<String>,
    ) -> Result<Self, WebAuthnHttpConfigurationError> {
        let required_role = required_role.into();
        let rp_origin = config.rp_origin().clone();
        validate_fixed_origin(&rp_origin)?;
        validate_role(&required_role)?;
        let credential_store_dyn: Arc<dyn WebAuthnCredentialStore> = credential_store;
        let service = Arc::new(WebAuthnCredentialEnrollmentService::new(
            config,
            credential_store_dyn.clone(),
            clock.clone(),
            authorizer,
        )?);
        let enrollment = Arc::new(ServiceEnrollmentPort {
            service,
            store: credential_store_dyn,
        });
        Ok(Self {
            rp_origin,
            required_role,
            enrollment: Some(enrollment),
            directory: Some(directory),
            session_bindings: Some(session_bindings),
            clock: Some(clock),
        })
    }
}

/// Axum router for protected SSO credential enrollment.
///
/// The caller must layer the same trusted BFF authentication middleware that
/// inserts [`VerifiedWebSessionContext`]. The route independently checks the
/// fixed `Origin`, the BFF-issued double-submit CSRF pair, session expiry,
/// Directory role, PostgreSQL verifier dependencies, and durable session
/// binding before returning or accepting any ceremony material.
pub fn webauthn_http_router(state: Arc<WebAuthnHttpState>) -> Router {
    Router::new()
        .route(
            "/v1/webauthn/credential-registrations",
            post(begin_credential_registration),
        )
        .route(
            "/v1/webauthn/credential-registrations/:registration_id/complete",
            post(complete_credential_registration),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

async fn begin_credential_registration(
    State(state): State<Arc<WebAuthnHttpState>>,
    session: Option<Extension<VerifiedWebSessionContext>>,
    headers: HeaderMap,
    payload: Result<
        Json<BeginCredentialRegistrationRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<(StatusCode, Json<BeginCredentialRegistrationResponse>), WebAuthnHttpError> {
    let session = required_session(session)?;
    authorize_http_request(&state, &session, &headers)?;
    let Json(request) = payload.map_err(|_| WebAuthnHttpError::InvalidRequest)?;
    validate_workspace_id(&request.workspace_id)?;
    let now = current_time(&state)?;
    ensure_session_fresh(&session, now)?;
    let (enrollment, directory, bindings) = required_enrollment_dependencies(&state)?;
    authorize_verified_web_session_role(
        directory.as_ref(),
        &session,
        &request.workspace_id,
        &state.required_role,
    )
    .await
    .map_err(map_http_authorization_error)?;

    let owner = session.principal.identity().clone();
    let enrollment_for_task = Arc::clone(&enrollment);
    let (challenge, stored_expiry) =
        task::spawn_blocking(move || enrollment_for_task.start_registration(&owner))
            .await
            .map_err(|_| WebAuthnHttpError::Unavailable)??;
    let principal_expiry = u64::try_from(session.principal.expires_at_unix_ms())
        .map_err(|_| WebAuthnHttpError::Unauthorized)?;
    let expires_at_unix_ms = stored_expiry.min(principal_expiry);
    if expires_at_unix_ms <= now {
        return Err(WebAuthnHttpError::Unavailable);
    }
    let binding = WebAuthnSessionCeremonyBinding::from_verified_session(
        WebAuthnHttpCeremonyPurpose::CredentialRegistration,
        challenge.registration_id,
        &session,
        request.workspace_id,
        expires_at_unix_ms,
    )
    .map_err(map_http_binding_error)?;
    bindings
        .bind_ceremony(binding)
        .await
        .map_err(map_http_binding_error)?;

    let options: Value = serde_json::from_slice(&challenge.credential_creation_options_json)
        .map_err(|_| WebAuthnHttpError::Unavailable)?;
    if !options.is_object() {
        return Err(WebAuthnHttpError::Unavailable);
    }
    Ok((
        StatusCode::CREATED,
        Json(BeginCredentialRegistrationResponse {
            registration_id: Uuid::from_bytes(challenge.registration_id).to_string(),
            webauthn_options: options,
            challenge_expires_at_unix_ms: expires_at_unix_ms,
        }),
    ))
}

async fn complete_credential_registration(
    State(state): State<Arc<WebAuthnHttpState>>,
    session: Option<Extension<VerifiedWebSessionContext>>,
    headers: HeaderMap,
    Path(registration_id): Path<String>,
    payload: Result<
        Json<CompleteCredentialRegistrationRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<StatusCode, WebAuthnHttpError> {
    let session = required_session(session)?;
    authorize_http_request(&state, &session, &headers)?;
    let Json(mut request) = payload.map_err(|_| WebAuthnHttpError::InvalidRequest)?;
    let registration_id = Uuid::parse_str(&registration_id)
        .map_err(|_| WebAuthnHttpError::InvalidRequest)?
        .into_bytes();
    let now = current_time(&state)?;
    ensure_session_fresh(&session, now)?;
    let (enrollment, directory, bindings) = required_enrollment_dependencies(&state)?;
    let active_binding = bindings
        .active_binding(
            WebAuthnHttpCeremonyPurpose::CredentialRegistration,
            &registration_id,
            &session,
            now,
        )
        .await
        .map_err(map_http_binding_error)?;
    validate_active_binding(
        &active_binding,
        WebAuthnHttpCeremonyPurpose::CredentialRegistration,
        &registration_id,
        &session,
        now,
    )?;
    authorize_verified_web_session_role(
        directory.as_ref(),
        &session,
        active_binding.workspace_id(),
        &state.required_role,
    )
    .await
    .map_err(map_http_authorization_error)?;

    let response_json = SecretBytes::serialize(&request.webauthn_registration)?;
    request.webauthn_registration.clear();
    if response_json.expose().is_empty()
        || response_json.expose().len() > MAX_REGISTRATION_RESPONSE_BYTES
    {
        return Err(WebAuthnHttpError::InvalidRequest);
    }
    let response_sha256: [u8; 32] = Sha256::digest(response_json.expose()).into();
    let reservation = bindings
        .reserve_finish(
            WebAuthnHttpCeremonyPurpose::CredentialRegistration,
            &registration_id,
            &session,
            response_sha256,
            now,
        )
        .await
        .map_err(map_http_binding_error)?;
    if reservation == WebAuthnSessionFinishReservation::AlreadyComplete {
        return Ok(StatusCode::NO_CONTENT);
    }

    let owner = session.principal.identity().clone();
    let registration_id_for_task = registration_id;
    let response = SecretBytes(response_json.expose().to_vec());
    let enrollment_for_task = Arc::clone(&enrollment);
    task::spawn_blocking(move || {
        enrollment_for_task.finish_registration(
            &owner,
            &registration_id_for_task,
            response.expose(),
        )
    })
    .await
    .map_err(|_| WebAuthnHttpError::Unavailable)??;
    bindings
        .complete_finish(
            WebAuthnHttpCeremonyPurpose::CredentialRegistration,
            &registration_id,
            &session,
            response_sha256,
            current_time(&state)?,
        )
        .await
        .map_err(map_http_binding_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn authorize_http_request(
    state: &WebAuthnHttpState,
    session: &VerifiedWebSessionContext,
    headers: &HeaderMap,
) -> Result<(), WebAuthnHttpError> {
    validate_origin(headers, &state.rp_origin)?;
    session.verify_csrf_headers(headers)
}

fn validate_origin(headers: &HeaderMap, fixed_origin: &Url) -> Result<(), WebAuthnHttpError> {
    let origin =
        exactly_one_header(headers, header::ORIGIN.as_str()).ok_or(WebAuthnHttpError::Forbidden)?;
    let origin = std::str::from_utf8(origin).map_err(|_| WebAuthnHttpError::Forbidden)?;
    let parsed = Url::parse(origin).map_err(|_| WebAuthnHttpError::Forbidden)?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.origin().ascii_serialization() != origin
        || parsed.origin() != fixed_origin.origin()
    {
        return Err(WebAuthnHttpError::Forbidden);
    }
    Ok(())
}

fn exactly_one_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a [u8]> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(value.as_bytes())
}

fn csrf_cookie(headers: &HeaderMap) -> Option<String> {
    let cookies: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
    if cookies.len() != 1 {
        return None;
    }
    let cookie_header = cookies[0].to_str().ok()?;
    let mut csrf_value = None;
    for part in cookie_header.split(';') {
        let (name, value) = part.trim().split_once('=')?;
        if name.trim() == CSRF_COOKIE_NAME {
            if csrf_value.is_some() || value.is_empty() || value.len() > 256 {
                return None;
            }
            csrf_value = Some(value.to_string());
        }
    }
    csrf_value
}

fn required_session(
    session: Option<Extension<VerifiedWebSessionContext>>,
) -> Result<VerifiedWebSessionContext, WebAuthnHttpError> {
    session
        .map(|Extension(session)| session)
        .ok_or(WebAuthnHttpError::Unauthorized)
}

fn validate_active_binding(
    binding: &WebAuthnSessionCeremonyBinding,
    purpose: WebAuthnHttpCeremonyPurpose,
    ceremony_id: &DeviceAuthorizationId,
    session: &VerifiedWebSessionContext,
    now_unix_ms: u64,
) -> Result<(), WebAuthnHttpError> {
    if binding.purpose() != purpose
        || binding.ceremony_id() != ceremony_id
        || binding.owner() != session.principal.identity()
        || binding.organization_id() != session.principal.organization_id()
        || !binding
            .session_binding()
            .constant_time_eq(session.session_binding())
        || binding.expires_at_unix_ms() <= now_unix_ms
    {
        return Err(WebAuthnHttpError::Forbidden);
    }
    validate_workspace_id(binding.workspace_id())
}

fn current_time(state: &WebAuthnHttpState) -> Result<u64, WebAuthnHttpError> {
    state
        .clock
        .as_ref()
        .ok_or(WebAuthnHttpError::Unavailable)?
        .current_unix_ms()
        .map_err(|_| WebAuthnHttpError::Unavailable)
}

fn ensure_session_fresh(
    session: &VerifiedWebSessionContext,
    now_unix_ms: u64,
) -> Result<(), WebAuthnHttpError> {
    if session.is_valid_at(now_unix_ms) {
        Ok(())
    } else {
        Err(WebAuthnHttpError::Unauthorized)
    }
}

fn required_enrollment_dependencies(
    state: &WebAuthnHttpState,
) -> Result<
    (
        Arc<dyn WebAuthnCredentialEnrollmentHttpPort>,
        Arc<dyn WorkspaceDirectory>,
        Arc<dyn WebAuthnHttpSessionBindingStore>,
    ),
    WebAuthnHttpError,
> {
    Ok((
        state
            .enrollment
            .as_ref()
            .cloned()
            .ok_or(WebAuthnHttpError::Unavailable)?,
        state
            .directory
            .as_ref()
            .cloned()
            .ok_or(WebAuthnHttpError::Unavailable)?,
        state
            .session_bindings
            .as_ref()
            .cloned()
            .ok_or(WebAuthnHttpError::Unavailable)?,
    ))
}

fn validate_fixed_origin(origin: &Url) -> Result<(), WebAuthnHttpConfigurationError> {
    if origin.scheme() != "https"
        || origin.host_str().is_none()
        || origin.username() != ""
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(WebAuthnHttpConfigurationError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_role(role: &str) -> Result<(), WebAuthnHttpConfigurationError> {
    if role.trim().is_empty() || role.len() > 128 || role.trim() != role {
        Err(WebAuthnHttpConfigurationError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

fn validate_workspace_id(workspace_id: &str) -> Result<(), WebAuthnHttpError> {
    if workspace_id.trim().is_empty()
        || workspace_id.len() > MAX_WORKSPACE_ID_BYTES
        || workspace_id.chars().any(char::is_control)
    {
        Err(WebAuthnHttpError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn map_http_authorization_error(error: WebAuthnHttpAuthorizationError) -> WebAuthnHttpError {
    match error {
        WebAuthnHttpAuthorizationError::Forbidden => WebAuthnHttpError::Forbidden,
        WebAuthnHttpAuthorizationError::Unavailable => WebAuthnHttpError::Unavailable,
    }
}

fn map_http_binding_error(error: WebAuthnHttpSessionBindingError) -> WebAuthnHttpError {
    match error {
        WebAuthnHttpSessionBindingError::NotFoundOrExpired => WebAuthnHttpError::Forbidden,
        WebAuthnHttpSessionBindingError::Conflict => WebAuthnHttpError::Conflict,
        WebAuthnHttpSessionBindingError::Unavailable => WebAuthnHttpError::Unavailable,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebAuthnHttpError {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    Conflict,
    Unavailable,
}

impl From<DeviceAuthorizationPortError> for WebAuthnHttpError {
    fn from(error: DeviceAuthorizationPortError) -> Self {
        match error {
            DeviceAuthorizationPortError::Rejected => Self::Forbidden,
            DeviceAuthorizationPortError::Unavailable => Self::Unavailable,
        }
    }
}

impl IntoResponse for WebAuthnHttpError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        };
        (status, Json(json!({ "error": code }))).into_response()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginCredentialRegistrationRequest {
    workspace_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BeginCredentialRegistrationResponse {
    registration_id: String,
    webauthn_options: Value,
    challenge_expires_at_unix_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteCredentialRegistrationRequest {
    webauthn_registration: SecretJsonValue,
}

struct SecretJsonValue(Value);

impl SecretJsonValue {
    fn clear(&mut self) {
        zeroize_json_strings(&mut self.0);
    }
}

impl<'de> Deserialize<'de> for SecretJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self)
    }
}

impl Drop for SecretJsonValue {
    fn drop(&mut self) {
        self.clear();
    }
}

struct SecretBytes(Vec<u8>);

impl SecretBytes {
    fn serialize(value: &SecretJsonValue) -> Result<Self, WebAuthnHttpError> {
        serde_json::to_vec(&value.0)
            .map(Self)
            .map_err(|_| WebAuthnHttpError::InvalidRequest)
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_strings),
        Value::Object(values) => values.values_mut().for_each(zeroize_json_strings),
        _ => {}
    }
}

fn update_hmac_field(mac: &mut HmacSha256, bytes: &[u8]) {
    mac.update(&(bytes.len() as u64).to_be_bytes());
    mac.update(bytes);
}

trait WebAuthnCredentialEnrollmentHttpPort: Send + Sync {
    fn start_registration(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<(WebAuthnCredentialRegistrationChallenge, u64), DeviceAuthorizationPortError>;

    fn finish_registration(
        &self,
        owner: &UserIdentityRef,
        registration_id: &DeviceAuthorizationId,
        response_json: &[u8],
    ) -> Result<(), DeviceAuthorizationPortError>;
}

struct ServiceEnrollmentPort {
    service: Arc<WebAuthnCredentialEnrollmentService>,
    store: Arc<dyn WebAuthnCredentialStore>,
}

impl WebAuthnCredentialEnrollmentHttpPort for ServiceEnrollmentPort {
    fn start_registration(
        &self,
        owner: &UserIdentityRef,
    ) -> Result<(WebAuthnCredentialRegistrationChallenge, u64), DeviceAuthorizationPortError> {
        let challenge = self.service.start_registration(owner)?;
        let registration = self
            .store
            .registration_ceremony(&challenge.registration_id)
            .map_err(map_credential_store_error)?
            .ok_or(DeviceAuthorizationPortError::Unavailable)?;
        if registration.owner != *owner {
            return Err(DeviceAuthorizationPortError::Unavailable);
        }
        Ok((challenge, registration.expires_at_unix_ms))
    }

    fn finish_registration(
        &self,
        owner: &UserIdentityRef,
        registration_id: &DeviceAuthorizationId,
        response_json: &[u8],
    ) -> Result<(), DeviceAuthorizationPortError> {
        self.service
            .finish_registration(owner, registration_id, response_json)
    }
}

fn map_credential_store_error(_: WebAuthnCredentialStoreError) -> DeviceAuthorizationPortError {
    DeviceAuthorizationPortError::Unavailable
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };

    use axum::{
        body::Body,
        http::{HeaderValue, Request, StatusCode},
    };
    use cy_proto::workspace_v1::UserIdentityRef;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::directory::WorkspaceDirectoryError;

    #[derive(Clone)]
    struct FixedClock(u64);

    impl DeviceAuthorizationClockPort for FixedClock {
        fn current_unix_ms(&self) -> Result<u64, DeviceAuthorizationPortError> {
            Ok(self.0)
        }
    }

    struct TestEnrollment {
        starts: AtomicUsize,
        finishes: AtomicUsize,
    }

    impl WebAuthnCredentialEnrollmentHttpPort for TestEnrollment {
        fn start_registration(
            &self,
            _owner: &UserIdentityRef,
        ) -> Result<(WebAuthnCredentialRegistrationChallenge, u64), DeviceAuthorizationPortError>
        {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok((
                WebAuthnCredentialRegistrationChallenge {
                    registration_id: [0x11; 16],
                    credential_creation_options_json:
                        br#"{"challenge":"server-created","timeout":300000}"#.to_vec(),
                },
                110_000,
            ))
        }

        fn finish_registration(
            &self,
            _owner: &UserIdentityRef,
            _registration_id: &DeviceAuthorizationId,
            response_json: &[u8],
        ) -> Result<(), DeviceAuthorizationPortError> {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            if response_json.is_empty() {
                Err(DeviceAuthorizationPortError::Rejected)
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct TestBindingStore {
        records: Mutex<
            BTreeMap<(WebAuthnHttpCeremonyPurpose, [u8; 16]), WebAuthnSessionCeremonyBinding>,
        >,
        reserved: Mutex<BTreeMap<(WebAuthnHttpCeremonyPurpose, [u8; 16]), ([u8; 32], bool)>>,
    }

    #[tonic::async_trait]
    impl WebAuthnHttpSessionBindingStore for TestBindingStore {
        async fn bind_ceremony(
            &self,
            binding: WebAuthnSessionCeremonyBinding,
        ) -> Result<(), WebAuthnHttpSessionBindingError> {
            self.records
                .lock()
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?
                .insert((binding.purpose(), *binding.ceremony_id()), binding);
            Ok(())
        }

        async fn active_binding(
            &self,
            purpose: WebAuthnHttpCeremonyPurpose,
            ceremony_id: &DeviceAuthorizationId,
            session: &VerifiedWebSessionContext,
            now_unix_ms: u64,
        ) -> Result<WebAuthnSessionCeremonyBinding, WebAuthnHttpSessionBindingError> {
            let records = self
                .records
                .lock()
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            let binding = records
                .get(&(purpose, *ceremony_id))
                .cloned()
                .ok_or(WebAuthnHttpSessionBindingError::NotFoundOrExpired)?;
            if binding.owner() != session.principal.identity()
                || binding.organization_id() != session.principal.organization_id()
                || !binding
                    .session_binding()
                    .constant_time_eq(session.session_binding())
                || binding.expires_at_unix_ms() <= now_unix_ms
            {
                return Err(WebAuthnHttpSessionBindingError::NotFoundOrExpired);
            }
            Ok(binding)
        }

        async fn reserve_finish(
            &self,
            purpose: WebAuthnHttpCeremonyPurpose,
            ceremony_id: &DeviceAuthorizationId,
            session: &VerifiedWebSessionContext,
            response_sha256: [u8; 32],
            now_unix_ms: u64,
        ) -> Result<WebAuthnSessionFinishReservation, WebAuthnHttpSessionBindingError> {
            self.active_binding(purpose, ceremony_id, session, now_unix_ms)
                .await?;
            let key = (purpose, *ceremony_id);
            let mut reserved = self
                .reserved
                .lock()
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            match reserved.get(&key) {
                Some((digest, complete)) if digest == &response_sha256 && *complete => {
                    Ok(WebAuthnSessionFinishReservation::AlreadyComplete)
                }
                Some((digest, _)) if digest == &response_sha256 => {
                    Ok(WebAuthnSessionFinishReservation::Continue)
                }
                Some(_) => Err(WebAuthnHttpSessionBindingError::Conflict),
                None => {
                    reserved.insert(key, (response_sha256, false));
                    Ok(WebAuthnSessionFinishReservation::Continue)
                }
            }
        }

        async fn complete_finish(
            &self,
            purpose: WebAuthnHttpCeremonyPurpose,
            ceremony_id: &DeviceAuthorizationId,
            session: &VerifiedWebSessionContext,
            response_sha256: [u8; 32],
            now_unix_ms: u64,
        ) -> Result<(), WebAuthnHttpSessionBindingError> {
            self.active_binding(purpose, ceremony_id, session, now_unix_ms)
                .await?;
            let mut reserved = self
                .reserved
                .lock()
                .map_err(|_| WebAuthnHttpSessionBindingError::Unavailable)?;
            match reserved.get_mut(&(purpose, *ceremony_id)) {
                Some((digest, complete)) if digest == &response_sha256 => {
                    *complete = true;
                    Ok(())
                }
                _ => Err(WebAuthnHttpSessionBindingError::Conflict),
            }
        }
    }

    #[tonic::async_trait]
    impl WorkspaceDirectory for TestDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<crate::workspace_v1::WorkspaceConnectionDescriptor>, WorkspaceDirectoryError>
        {
            Ok(Vec::new())
        }

        async fn is_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(self.allowed)
        }

        async fn roles_for_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<Option<BTreeSet<String>>, WorkspaceDirectoryError> {
            Ok(self.allowed.then(|| self.roles.clone()))
        }
    }

    struct TestDirectory {
        allowed: bool,
        roles: BTreeSet<String>,
    }

    struct TestStateParts {
        enrollment: Arc<dyn WebAuthnCredentialEnrollmentHttpPort>,
        directory: Arc<dyn WorkspaceDirectory>,
        session_bindings: Arc<dyn WebAuthnHttpSessionBindingStore>,
        clock: Arc<dyn DeviceAuthorizationClockPort>,
    }

    fn test_state(parts: TestStateParts) -> Arc<WebAuthnHttpState> {
        Arc::new(WebAuthnHttpState {
            rp_origin: Url::parse("https://workspace.example").unwrap(),
            required_role: WORKSPACE_MEMBER_ROLE.into(),
            enrollment: Some(parts.enrollment),
            directory: Some(parts.directory),
            session_bindings: Some(parts.session_bindings),
            clock: Some(parts.clock),
        })
    }

    fn principal(expiry: i64) -> VerifiedWebPrincipal {
        // The public verifier has network discovery requirements. The test-only
        // constructor is below in the web_identity module and is intentionally
        // not present in production.
        crate::web_identity::test_principal(user(), "org-authoritative", expiry)
    }

    fn user() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://issuer.example/tenant/v2.0".into(),
            subject: "subject-123".into(),
        }
    }

    fn session(access_token: &str, expiry: i64) -> VerifiedWebSessionContext {
        VerifiedWebSessionContext::from_bff_verified_access_token(
            principal(expiry),
            access_token,
            &[0x24; 32],
            "v1.test-csrf-token",
        )
        .unwrap()
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://workspace.example"),
        );
        headers.insert(
            "x-csrf-token",
            HeaderValue::from_static("v1.test-csrf-token"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("__Secure-cyrene-csrf=v1.test-csrf-token"),
        );
        headers
    }

    fn role_directory() -> Arc<dyn WorkspaceDirectory> {
        Arc::new(TestDirectory {
            allowed: true,
            roles: BTreeSet::from([WORKSPACE_MEMBER_ROLE.into()]),
        })
    }

    fn test_parts(
        enrollment: Arc<TestEnrollment>,
        directory: Arc<dyn WorkspaceDirectory>,
        bindings: Arc<TestBindingStore>,
    ) -> TestStateParts {
        TestStateParts {
            enrollment,
            directory,
            session_bindings: bindings,
            clock: Arc::new(FixedClock(100_000)),
        }
    }

    async fn post_json(
        router: Router,
        path: &str,
        session: Option<VerifiedWebSessionContext>,
        headers: HeaderMap,
        body: Value,
    ) -> Response {
        let mut builder = Request::builder().method("POST").uri(path);
        for (name, value) in headers {
            if let Some(name) = name {
                builder = builder.header(name, value);
            }
        }
        let request = builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        if let Some(session) = session {
            router
                .layer(Extension(session))
                .oneshot(request)
                .await
                .unwrap()
        } else {
            router.oneshot(request).await.unwrap()
        }
    }

    #[tokio::test]
    async fn missing_production_dependencies_fail_closed_before_challenge_creation() {
        let state = WebAuthnHttpState::unavailable(
            Url::parse("https://workspace.example").unwrap(),
            WORKSPACE_MEMBER_ROLE,
        )
        .unwrap();
        let response = post_json(
            webauthn_http_router(Arc::new(state)),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            headers(),
            json!({ "workspaceId": "ws-1" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn origin_csrf_and_directory_role_are_required_before_registration() {
        let enrollment = Arc::new(TestEnrollment {
            starts: AtomicUsize::new(0),
            finishes: AtomicUsize::new(0),
        });
        let bindings = Arc::new(TestBindingStore::default());
        let directory: Arc<dyn WorkspaceDirectory> = Arc::new(TestDirectory {
            allowed: true,
            roles: BTreeSet::new(),
        });
        let state = test_state(test_parts(enrollment.clone(), directory, bindings));
        let mut invalid_origin = headers();
        invalid_origin.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        let response = post_json(
            webauthn_http_router(Arc::clone(&state)),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            invalid_origin,
            json!({ "workspaceId": "ws-1" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let state = test_state(test_parts(
            enrollment.clone(),
            role_directory(),
            Arc::new(TestBindingStore::default()),
        ));
        let mut invalid_csrf = headers();
        invalid_csrf.insert("x-csrf-token", HeaderValue::from_static("different"));
        let response = post_json(
            webauthn_http_router(state),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            invalid_csrf,
            json!({ "workspaceId": "ws-1" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let state = test_state(test_parts(
            enrollment.clone(),
            Arc::new(TestDirectory {
                allowed: false,
                roles: BTreeSet::from([WORKSPACE_MEMBER_ROLE.into()]),
            }),
            Arc::new(TestBindingStore::default()),
        ));
        let response = post_json(
            webauthn_http_router(state),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            headers(),
            json!({ "workspaceId": "ws-1" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(enrollment.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn begin_uses_verified_owner_and_org_and_finish_rejects_refresh_session() {
        let enrollment = Arc::new(TestEnrollment {
            starts: AtomicUsize::new(0),
            finishes: AtomicUsize::new(0),
        });
        let bindings = Arc::new(TestBindingStore::default());
        let state = test_state(test_parts(
            enrollment.clone(),
            role_directory(),
            bindings.clone(),
        ));
        let begin = post_json(
            webauthn_http_router(Arc::clone(&state)),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            headers(),
            json!({ "workspaceId": "ws-1", "organizationId": "attacker-org", "owner": {"issuer":"x","subject":"y"}, "role":"workspace.admin" }),
        )
        .await;
        // Unknown authorization-bearing fields are rejected, rather than trusted.
        assert_eq!(begin.status(), StatusCode::BAD_REQUEST);
        assert_eq!(enrollment.starts.load(Ordering::SeqCst), 0);

        let begin = post_json(
            webauthn_http_router(Arc::clone(&state)),
            "/v1/webauthn/credential-registrations",
            Some(session("bearer-a", 200_000)),
            headers(),
            json!({ "workspaceId": "ws-1" }),
        )
        .await;
        assert_eq!(begin.status(), StatusCode::CREATED);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(begin.into_body(), MAX_HTTP_BODY_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            body["registrationId"],
            Uuid::from_bytes([0x11; 16]).to_string()
        );
        assert_eq!(body["challengeExpiresAtUnixMs"], 110_000);
        assert_eq!(body["webauthnOptions"]["challenge"], "server-created");

        let finish = post_json(
            webauthn_http_router(state),
            &format!(
                "/v1/webauthn/credential-registrations/{}/complete",
                Uuid::from_bytes([0x11; 16])
            ),
            Some(session("bearer-after-refresh", 200_000)),
            headers(),
            json!({ "webauthnRegistration": { "id": "credential" } }),
        )
        .await;
        assert_eq!(finish.status(), StatusCode::FORBIDDEN);
        assert_eq!(enrollment.finishes.load(Ordering::SeqCst), 0);

        let finish = post_json(
            webauthn_http_router(test_state(test_parts(
                enrollment.clone(),
                role_directory(),
                bindings,
            ))),
            &format!(
                "/v1/webauthn/credential-registrations/{}/complete",
                Uuid::from_bytes([0x11; 16])
            ),
            Some(session("bearer-a", 200_000)),
            headers(),
            json!({ "webauthnRegistration": { "id": "credential" } }),
        )
        .await;
        assert_eq!(finish.status(), StatusCode::NO_CONTENT);
        assert_eq!(enrollment.finishes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn bearer_refresh_changes_session_binding_and_debug_redacts_it() {
        let first = session("bearer-a", 200_000);
        let refreshed = session("bearer-b", 200_000);
        assert!(!first
            .session_binding()
            .constant_time_eq(refreshed.session_binding()));
        assert!(!format!("{first:?}").contains("bearer-a"));
        assert!(!format!("{first:?}").contains("v1.test-csrf-token"));
    }

    #[test]
    fn exact_https_origin_and_strict_role_are_required() {
        let fixed = Url::parse("https://workspace.example").unwrap();
        let mut values = headers();
        values.append(
            header::ORIGIN,
            HeaderValue::from_static("https://workspace.example"),
        );
        assert_eq!(
            validate_origin(&values, &fixed),
            Err(WebAuthnHttpError::Forbidden)
        );
        assert!(validate_fixed_origin(&Url::parse("http://workspace.example").unwrap()).is_err());
        assert!(validate_role("").is_err());
    }
}
