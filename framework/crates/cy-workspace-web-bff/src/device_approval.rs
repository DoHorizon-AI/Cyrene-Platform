// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/device_approval.rs  ║
// ║ Module: cy_workspace_web_bff::device_approval                      ║
// ║ Role: Expose the three authenticated browser approval operations.   ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::device_approval                        ║
// ║ 职责：提供三个已认证的浏览器审批操作。                               ║
// ╚══════════════════════════════════════════════════════════════════════╝

//! Same-origin browser routes for Workspace device approval.
//!
//! The host must insert [`VerifiedWebSessionContext`] only after validating
//! the access token, exact Origin, and session-bound CSRF MAC. This module
//! rechecks the dedicated Directory capability and persists WebAuthn ceremony
//! bindings; a missing backend fails closed with a fixed problem response.
//!
//! Workspace 设备审批的同源浏览器路由。Host 只有在验证 access token、精确 Origin 与 session 绑定 CSRF MAC 后，
//! 才能注入 [`VerifiedWebSessionContext`]。本模块重新检查专用 Directory capability 并持久化 WebAuthn ceremony
//! 绑定；后端缺失时以固定 problem response fail closed。

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::DateTime;
use cy_workspace_fabric::{
    authorize_verified_web_session_role, VerifiedWebPrincipal, VerifiedWebSessionContext,
    WebAuthnHttpAuthorizationError, WebAuthnHttpCeremonyPurpose, WebAuthnHttpSessionBindingError,
    WebAuthnHttpSessionBindingStore, WebAuthnSessionCeremonyBinding,
    WebAuthnSessionFinishReservation, WorkspaceDirectory, WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE,
};
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use http::HeaderValue as HttpHeaderValue;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    http::MAX_JSON_BODY_BYTES,
    problem::{problem_response, ProblemCode},
};

const MAX_APPROVAL_ID_BYTES: usize = 128;
const MIN_APPROVAL_ID_BYTES: usize = 16;
const MAX_USER_CODE_BYTES: usize = 128;
const MAX_ASSERTION_BYTES: usize = 64 * 1024;
const MAX_ACCEPT_HEADER_BYTES: usize = 512;
const APPROVAL_ID_DOMAIN: &[u8] = b"cyrene.workspace.web.device-approval-id.v1\0";

/// Workspace selector supplied by the browser; organization must match the
/// server-side organization resolved from the verified access token.
///
/// 浏览器提供的 Workspace 选择器；organization 必须与已验证 access token 的服务端映射一致。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceApprovalScope {
    /// Exact Directory organization identifier.
    /// Directory 中精确的 organization 标识。
    pub organization_id: String,
    /// Exact Directory Workspace identifier.
    /// Directory 中精确的 Workspace 标识。
    pub workspace_id: String,
}

/// Fixed categories returned by the trusted Device Authorization adapter.
///
/// 可信 Device Authorization adapter 返回的固定错误类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceApprovalServiceError {
    /// Request data is invalid or a WebAuthn assertion was rejected.
    /// 请求无效或 WebAuthn assertion 被拒绝。
    InvalidRequest,
    /// The verified principal lacks authority for the exact authorization.
    /// 已验证 principal 对该授权范围没有权限。
    Forbidden,
    /// The authorization or opaque code is absent or inaccessible.
    /// 授权或不透明 code 不存在或不可访问。
    NotFound,
    /// The durable authorization is terminal or conflicts with this request.
    /// 持久授权已结束，或与本次请求冲突。
    Conflict,
    /// A configured attempt limit was reached.
    /// 已达到配置的尝试次数限制。
    RateLimited,
    /// A required durable provider is unavailable.
    /// 所需的持久化 provider 当前不可用。
    Unavailable,
    /// The adapter returned an internal failure without a safe public category.
    /// adapter 返回了无法安全公开的内部失败。
    Internal,
}

/// Canonical result from a Device Authorization completion operation.
///
/// Device Authorization 完成操作的规范结果。
pub struct DeviceApprovalCompletion {
    /// Closed canonical `CompleteDeviceApprovalResponse` JSON object.
    /// 封闭且符合规范的 `CompleteDeviceApprovalResponse` JSON 对象。
    pub body: Value,
    /// False only when this call durably commits the first ISSUING transition.
    /// 仅当本次调用首次持久提交 ISSUING transition 时为 false。
    pub accepted: bool,
}

/// Narrow server-only adapter for the durable Device Authorization authority.
///
/// Implementations must derive approver identity only from `principal`, never
/// from request JSON. `complete` must accept an omitted assertion only when
/// the authorization is already durably ISSUING or later. Raw assertion JSON,
/// `user_code`, and `abuse_key` must not be logged or persisted. `deny` must
/// invalidate an active approval challenge in the same durable authorization
/// transaction.
///
/// 面向持久 Device Authorization authority 的窄 server-only adapter。实现必须只从 `principal` 派生审批人身份，
/// 不得从请求 JSON 读取；`complete` 只有在授权已持久进入 ISSUING 或更后状态时才可接受省略的 assertion。Raw assertion JSON、
/// `user_code` 与 `abuse_key` 不得记录或持久化；`deny` 必须在同一持久授权事务中使活动审批 challenge 失效。
#[async_trait]
pub trait DeviceApprovalService: Send + Sync {
    /// Resolve a one-time user code and create or recover its WebAuthn challenge.
    /// 解析一次性 user code，并创建或恢复对应的 WebAuthn challenge。
    async fn begin(
        &self,
        principal: &VerifiedWebPrincipal,
        abuse_key: [u8; 32],
        user_code: &str,
        scope: &DeviceApprovalScope,
    ) -> Result<Value, DeviceApprovalServiceError>;

    /// Verify an assertion or recover an already durable issuance.
    /// 验证 assertion，或恢复已经持久化的签发流程。
    async fn complete(
        &self,
        principal: &VerifiedWebPrincipal,
        approval_id: &str,
        assertion: Option<Value>,
    ) -> Result<DeviceApprovalCompletion, DeviceApprovalServiceError>;

    /// Deny one exact authorization and invalidate its active approval challenge.
    /// 拒绝一个精确范围内的授权，并使其活动审批 challenge 失效。
    async fn deny(
        &self,
        principal: &VerifiedWebPrincipal,
        abuse_key: [u8; 32],
        user_code: &str,
        scope: &DeviceApprovalScope,
    ) -> Result<Value, DeviceApprovalServiceError>;
}

/// Runtime providers for the browser approval routes.
///
/// Leave any provider as `None` until its production implementation is ready;
/// every route then returns a fixed 503 response.
///
/// 浏览器审批路由的运行时 provider。生产实现就绪前可将任一 provider 设为 `None`；此时所有路由固定返回 503。
#[derive(Clone, Default)]
pub struct DeviceApprovalDependencies {
    /// Durable Device Authorization and WebAuthn adapter.
    /// 持久 Device Authorization 与 WebAuthn adapter。
    pub service: Option<Arc<dyn DeviceApprovalService>>,
    /// Authoritative Directory membership and role lookup.
    /// 权威 Directory membership 与 role 查询。
    pub directory: Option<Arc<dyn WorkspaceDirectory>>,
    /// Durable WebAuthn HTTP session-to-ceremony binding store.
    /// 持久 WebAuthn HTTP session-to-ceremony 绑定存储。
    pub session_bindings: Option<Arc<dyn WebAuthnHttpSessionBindingStore>>,
}

/// Create only the three documented same-origin browser approval routes.
///
/// This router does not mount device start, poll, delivery, or acknowledgement.
/// The containing BFF must attach its Bearer, Origin, and CSRF verification
/// before inserting `VerifiedWebSessionContext` into any request.
///
/// 仅创建合同规定的三个同源浏览器审批路由。本 router 不挂载设备 start、poll、delivery 或 acknowledgement。
/// 外层 BFF 必须先验证 Bearer、Origin 与 CSRF，再向请求注入 `VerifiedWebSessionContext`。
pub fn device_approval_router(dependencies: DeviceApprovalDependencies) -> Router {
    Router::new()
        .route(
            "/api/workspace/v1/device-authorizations/approval-challenges",
            post(begin_approval),
        )
        .route(
            "/api/workspace/v1/device-authorizations/approval-challenges/:approval_id/complete",
            post(complete_approval),
        )
        .route(
            "/api/workspace/v1/device-authorizations/denials",
            post(deny_authorization),
        )
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
        .with_state(dependencies)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginRequest {
    user_code: String,
    scope: DeviceApprovalScope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteRequest {
    #[serde(default)]
    webauthn_assertion: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DenyRequest {
    user_code: String,
    scope: DeviceApprovalScope,
}

async fn begin_approval(
    State(dependencies): State<DeviceApprovalDependencies>,
    session: Option<Extension<VerifiedWebSessionContext>>,
    headers: HeaderMap,
    payload: Result<Json<BeginRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApprovalHttpError> {
    let session = require_session(session)?;
    let now = now_unix_ms()?;
    require_fresh_session(&session, now)?;
    validate_json_headers(&headers)?;
    let Json(request) = parse_json(payload)?;
    validate_scope(&request.scope)?;
    validate_user_code(&request.user_code)?;
    let principal = session.principal();
    if request.scope.organization_id != principal.organization_id() {
        return Err(ApprovalHttpError::new(ProblemCode::Forbidden, "Forbidden"));
    }
    let (service, directory, session_bindings) = required_dependencies(&dependencies)?;
    authorize_scope(directory.as_ref(), &session, &request.scope.workspace_id).await?;

    let body = service
        .begin(
            principal,
            *session.session_binding().as_bytes(),
            &request.user_code,
            &request.scope,
        )
        .await
        .map_err(map_service_error)?;
    ensure_response_size(&body)?;
    let (approval_id, ceremony_id, expires_at_unix_ms) =
        validate_begin_response(&body, &request.scope, now, principal.expires_at_unix_ms())?;
    let binding = WebAuthnSessionCeremonyBinding::from_verified_session(
        WebAuthnHttpCeremonyPurpose::DeviceApproval,
        ceremony_id,
        &session,
        request.scope.workspace_id.clone(),
        expires_at_unix_ms,
    )
    .map_err(map_binding_error)?;
    match session_bindings.bind_ceremony(binding).await {
        Ok(()) => {}
        Err(WebAuthnHttpSessionBindingError::Conflict) => {
            let existing = session_bindings
                .active_binding(
                    WebAuthnHttpCeremonyPurpose::DeviceApproval,
                    &ceremony_id_from_approval_id(&approval_id),
                    &session,
                    now,
                )
                .await
                .map_err(map_binding_error)?;
            if existing.owner() != principal.identity()
                || existing.organization_id() != principal.organization_id()
                || existing.workspace_id() != request.scope.workspace_id
                || existing.expires_at_unix_ms() != expires_at_unix_ms
            {
                return Err(ApprovalHttpError::new(
                    ProblemCode::StateConflict,
                    "Conflict",
                ));
            }
        }
        Err(error) => return Err(map_binding_error(error)),
    }
    Ok(json_response(StatusCode::OK, body))
}

async fn complete_approval(
    State(dependencies): State<DeviceApprovalDependencies>,
    session: Option<Extension<VerifiedWebSessionContext>>,
    headers: HeaderMap,
    Path(approval_id): Path<String>,
    payload: Result<Json<CompleteRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApprovalHttpError> {
    let session = require_session(session)?;
    let now = now_unix_ms()?;
    require_fresh_session(&session, now)?;
    validate_json_headers(&headers)?;
    validate_approval_id(&approval_id)?;
    let Json(request) = parse_json(payload)?;
    let assertion_bytes = request
        .webauthn_assertion
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| ApprovalHttpError::new(ProblemCode::InvalidRequest, "Bad Request"))?;
    if assertion_bytes
        .as_ref()
        .is_some_and(|bytes| bytes.is_empty() || bytes.len() > MAX_ASSERTION_BYTES)
    {
        return Err(ApprovalHttpError::new(
            ProblemCode::InvalidRequest,
            "Bad Request",
        ));
    }

    let (service, directory, session_bindings) = required_dependencies(&dependencies)?;
    let principal = session.principal();
    let ceremony_id = ceremony_id_from_approval_id(&approval_id);
    let binding = session_bindings
        .active_binding(
            WebAuthnHttpCeremonyPurpose::DeviceApproval,
            &ceremony_id,
            &session,
            now,
        )
        .await
        .map_err(map_binding_error)?;
    if binding.owner() != principal.identity()
        || binding.organization_id() != principal.organization_id()
        || binding.expires_at_unix_ms() <= now
    {
        return Err(ApprovalHttpError::new(
            ProblemCode::WorkspaceNotFound,
            "Not Found",
        ));
    }
    authorize_scope(directory.as_ref(), &session, binding.workspace_id()).await?;

    let assertion_finish = if let Some(bytes) = assertion_bytes.as_ref() {
        let digest = Sha256::digest(bytes);
        let mut response_sha256 = [0_u8; 32];
        response_sha256.copy_from_slice(&digest);
        let reservation = session_bindings
            .reserve_finish(
                WebAuthnHttpCeremonyPurpose::DeviceApproval,
                &ceremony_id,
                &session,
                response_sha256,
                now,
            )
            .await
            .map_err(map_binding_error)?;
        Some((
            response_sha256,
            reservation == WebAuthnSessionFinishReservation::AlreadyComplete,
        ))
    } else {
        None
    };

    let completion = service
        .complete(principal, &approval_id, request.webauthn_assertion)
        .await
        .map_err(map_service_error)?;
    ensure_response_size(&completion.body)?;
    let state = validate_complete_response(&completion.body, principal, &binding)?;
    if completion.accepted == (state == "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING") {
        return Err(ApprovalHttpError::new(
            ProblemCode::InvalidUpstreamResponse,
            "Bad Gateway",
        ));
    }
    if let Some((response_sha256, _)) = assertion_finish {
        session_bindings
            .complete_finish(
                WebAuthnHttpCeremonyPurpose::DeviceApproval,
                &ceremony_id,
                &session,
                response_sha256,
                now_unix_ms()?,
            )
            .await
            .map_err(map_binding_error)?;
    }
    let status = if assertion_finish.is_none()
        || assertion_finish.is_some_and(|(_, already_complete)| already_complete)
        || completion.accepted
    {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok(json_response(status, completion.body))
}

async fn deny_authorization(
    State(dependencies): State<DeviceApprovalDependencies>,
    session: Option<Extension<VerifiedWebSessionContext>>,
    headers: HeaderMap,
    payload: Result<Json<DenyRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApprovalHttpError> {
    let session = require_session(session)?;
    let now = now_unix_ms()?;
    require_fresh_session(&session, now)?;
    validate_json_headers(&headers)?;
    let Json(request) = parse_json(payload)?;
    validate_scope(&request.scope)?;
    validate_user_code(&request.user_code)?;
    let principal = session.principal();
    if request.scope.organization_id != principal.organization_id() {
        return Err(ApprovalHttpError::new(ProblemCode::Forbidden, "Forbidden"));
    }
    let (service, directory) = required_service_and_directory(&dependencies)?;
    authorize_scope(directory.as_ref(), &session, &request.scope.workspace_id).await?;
    let body = service
        .deny(
            principal,
            *session.session_binding().as_bytes(),
            &request.user_code,
            &request.scope,
        )
        .await
        .map_err(map_service_error)?;
    ensure_response_size(&body)?;
    validate_denial_response(&body, principal, &request.scope)?;
    Ok(json_response(StatusCode::OK, body))
}

type RequiredProviders = (
    Arc<dyn DeviceApprovalService>,
    Arc<dyn WorkspaceDirectory>,
    Arc<dyn WebAuthnHttpSessionBindingStore>,
);

fn required_dependencies(
    dependencies: &DeviceApprovalDependencies,
) -> Result<RequiredProviders, ApprovalHttpError> {
    let (service, directory) = required_service_and_directory(dependencies)?;
    Ok((
        service,
        directory,
        dependencies
            .session_bindings
            .as_ref()
            .cloned()
            .ok_or_else(unavailable)?,
    ))
}

fn required_service_and_directory(
    dependencies: &DeviceApprovalDependencies,
) -> Result<(Arc<dyn DeviceApprovalService>, Arc<dyn WorkspaceDirectory>), ApprovalHttpError> {
    Ok((
        dependencies
            .service
            .as_ref()
            .cloned()
            .ok_or_else(unavailable)?,
        dependencies
            .directory
            .as_ref()
            .cloned()
            .ok_or_else(unavailable)?,
    ))
}

async fn authorize_scope(
    directory: &dyn WorkspaceDirectory,
    session: &VerifiedWebSessionContext,
    workspace_id: &str,
) -> Result<(), ApprovalHttpError> {
    authorize_verified_web_session_role(
        directory,
        session,
        workspace_id,
        WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE,
    )
    .await
    .map_err(|error| match error {
        WebAuthnHttpAuthorizationError::Forbidden => {
            ApprovalHttpError::new(ProblemCode::Forbidden, "Forbidden")
        }
        WebAuthnHttpAuthorizationError::Unavailable => unavailable(),
    })
}

fn require_session(
    session: Option<Extension<VerifiedWebSessionContext>>,
) -> Result<VerifiedWebSessionContext, ApprovalHttpError> {
    session
        .map(|Extension(session)| session)
        .ok_or_else(|| ApprovalHttpError::new(ProblemCode::Unauthenticated, "Unauthorized"))
}

fn require_fresh_session(
    session: &VerifiedWebSessionContext,
    now_unix_ms: u64,
) -> Result<(), ApprovalHttpError> {
    if !session.is_valid_at(now_unix_ms) {
        return Err(ApprovalHttpError::new(
            ProblemCode::Unauthenticated,
            "Unauthorized",
        ));
    }
    Ok(())
}

fn now_unix_ms() -> Result<u64, ApprovalHttpError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApprovalHttpError::new(ProblemCode::InternalError, "Internal Server Error"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| ApprovalHttpError::new(ProblemCode::InternalError, "Internal Server Error"))
}

fn validate_json_headers(headers: &HeaderMap) -> Result<(), ApprovalHttpError> {
    let content_types = headers
        .get_all(header::CONTENT_TYPE)
        .iter()
        .collect::<Vec<_>>();
    if content_types.len() != 1 {
        return Err(ApprovalHttpError::new(
            ProblemCode::UnsupportedMediaType,
            "Unsupported Media Type",
        ));
    }
    let content_type = content_types[0].to_str().map_err(|_| {
        ApprovalHttpError::new(ProblemCode::UnsupportedMediaType, "Unsupported Media Type")
    })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/json") {
        return Err(ApprovalHttpError::new(
            ProblemCode::UnsupportedMediaType,
            "Unsupported Media Type",
        ));
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter();
    if let Some(length) = lengths.next() {
        if lengths.next().is_some() {
            return Err(ApprovalHttpError::new(
                ProblemCode::InvalidRequest,
                "Bad Request",
            ));
        }
        let length = length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| ApprovalHttpError::new(ProblemCode::InvalidRequest, "Bad Request"))?;
        if length > MAX_JSON_BODY_BYTES {
            return Err(ApprovalHttpError::new(
                ProblemCode::PayloadTooLarge,
                "Payload Too Large",
            ));
        }
    }
    if !accepts_json(headers) {
        return Err(ApprovalHttpError::with_status(
            StatusCode::NOT_ACCEPTABLE,
            ProblemCode::InvalidRequest,
            "Not Acceptable",
        ));
    }
    Ok(())
}

fn accepts_json(headers: &HeaderMap) -> bool {
    let values = headers.get_all(header::ACCEPT).iter().collect::<Vec<_>>();
    if values.is_empty() {
        return true;
    }
    let mut combined_length = 0;
    for value in values {
        let Ok(value) = value.to_str() else {
            return false;
        };
        combined_length += value.len();
        if combined_length > MAX_ACCEPT_HEADER_BYTES {
            return false;
        }
        for range in value.split(',') {
            let mut parts = range.trim().split(';');
            let media_type = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
            let mut quality = 1.0_f32;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        quality = value.trim().parse::<f32>().unwrap_or(0.0);
                    }
                }
            }
            if quality > 0.0
                && matches!(
                    media_type.as_str(),
                    "*/*" | "application/*" | "application/json" | "application/problem+json"
                )
            {
                return true;
            }
        }
    }
    false
}

fn parse_json<T>(
    payload: Result<Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<T>, ApprovalHttpError> {
    payload.map_err(|rejection| {
        let status = rejection.into_response().status();
        match status {
            StatusCode::PAYLOAD_TOO_LARGE => {
                ApprovalHttpError::new(ProblemCode::PayloadTooLarge, "Payload Too Large")
            }
            StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                ApprovalHttpError::new(ProblemCode::UnsupportedMediaType, "Unsupported Media Type")
            }
            _ => ApprovalHttpError::new(ProblemCode::InvalidRequest, "Bad Request"),
        }
    })
}

fn validate_scope(scope: &DeviceApprovalScope) -> Result<(), ApprovalHttpError> {
    if scope.organization_id.trim().is_empty()
        || scope.organization_id.len() > 256
        || scope.workspace_id.trim().is_empty()
        || scope.workspace_id.len() > 128
        || scope
            .organization_id
            .chars()
            .chain(scope.workspace_id.chars())
            .any(char::is_control)
    {
        return Err(ApprovalHttpError::new(
            ProblemCode::InvalidRequest,
            "Bad Request",
        ));
    }
    Ok(())
}

fn validate_user_code(user_code: &str) -> Result<(), ApprovalHttpError> {
    if user_code.trim().is_empty()
        || user_code.len() > MAX_USER_CODE_BYTES
        || user_code.chars().any(char::is_control)
    {
        return Err(ApprovalHttpError::new(
            ProblemCode::InvalidRequest,
            "Bad Request",
        ));
    }
    Ok(())
}

fn validate_approval_id(approval_id: &str) -> Result<(), ApprovalHttpError> {
    if !(MIN_APPROVAL_ID_BYTES..=MAX_APPROVAL_ID_BYTES).contains(&approval_id.len())
        || !approval_id.is_ascii()
        || approval_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ApprovalHttpError::new(
            ProblemCode::InvalidRequest,
            "Bad Request",
        ));
    }
    Ok(())
}

fn validate_begin_response(
    value: &Value,
    expected_scope: &DeviceApprovalScope,
    now_unix_ms: u64,
    principal_expiry_unix_ms: i64,
) -> Result<(String, [u8; 16], u64), ApprovalHttpError> {
    if !has_exact_keys(
        value,
        &[
            "authorization",
            "approvalId",
            "webauthnOptions",
            "challengeExpiresAt",
        ],
    ) {
        return Err(invalid_upstream());
    }
    let approval_id = string_field(value, "approvalId")?;
    validate_approval_id(approval_id).map_err(|_| invalid_upstream())?;
    validate_webauthn_options(value.get("webauthnOptions").ok_or_else(invalid_upstream)?)?;
    let authorization = value.get("authorization").ok_or_else(invalid_upstream)?;
    validate_authorization_ref(authorization, expected_scope)?;
    let authorization_expiry = parse_datetime_ms(string_field(authorization, "expiresAt")?)?;
    let expires_at = parse_datetime_ms(string_field(value, "challengeExpiresAt")?)?;
    let principal_expiry =
        u64::try_from(principal_expiry_unix_ms).map_err(|_| invalid_upstream())?;
    if expires_at <= now_unix_ms
        || expires_at > principal_expiry
        || authorization_expiry <= now_unix_ms
        || expires_at > authorization_expiry
    {
        return Err(invalid_upstream());
    }
    let ceremony_id = ceremony_id_from_approval_id(approval_id);
    if ceremony_id.iter().all(|byte| *byte == 0) {
        return Err(invalid_upstream());
    }
    Ok((approval_id.to_owned(), ceremony_id, expires_at))
}

fn validate_complete_response(
    value: &Value,
    principal: &VerifiedWebPrincipal,
    binding: &WebAuthnSessionCeremonyBinding,
) -> Result<&'static str, ApprovalHttpError> {
    if !has_exact_keys(
        value,
        &["authorization", "state", "approvedBy", "approvedAt"],
    ) || !validate_authorization_ref(
        value.get("authorization").ok_or_else(invalid_upstream)?,
        &DeviceApprovalScope {
            organization_id: binding.organization_id().to_owned(),
            workspace_id: binding.workspace_id().to_owned(),
        },
    )
    .is_ok()
        || !validate_identity(
            value.get("approvedBy").ok_or_else(invalid_upstream)?,
            principal,
        )?
    {
        return Err(invalid_upstream());
    }
    parse_datetime_ms(string_field(value, "approvedAt")?).map_err(|_| invalid_upstream())?;
    match string_field(value, "state")? {
        "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING" => {
            Ok("DEVICE_AUTHORIZATION_LIFECYCLE_STATE_ISSUING")
        }
        "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERY_PENDING" => {
            Ok("DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERY_PENDING")
        }
        "DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED" => {
            Ok("DEVICE_AUTHORIZATION_LIFECYCLE_STATE_DELIVERED")
        }
        _ => Err(invalid_upstream()),
    }
}

fn validate_denial_response(
    value: &Value,
    principal: &VerifiedWebPrincipal,
    expected_scope: &DeviceApprovalScope,
) -> Result<(), ApprovalHttpError> {
    if !has_exact_keys(value, &["authorization", "deniedBy", "deniedAt"]) {
        return Err(invalid_upstream());
    }
    validate_authorization_ref(
        value.get("authorization").ok_or_else(invalid_upstream)?,
        expected_scope,
    )?;
    if !validate_identity(
        value.get("deniedBy").ok_or_else(invalid_upstream)?,
        principal,
    )? {
        return Err(invalid_upstream());
    }
    parse_datetime_ms(string_field(value, "deniedAt")?).map_err(|_| invalid_upstream())?;
    Ok(())
}

fn validate_authorization_ref(
    value: &Value,
    expected_scope: &DeviceApprovalScope,
) -> Result<(), ApprovalHttpError> {
    if !has_exact_keys(
        value,
        &[
            "authorizationId",
            "deviceId",
            "scope",
            "csrSpkiSha256",
            "csrSha256",
            "expiresAt",
            "authorizationGeneration",
        ],
    ) {
        return Err(invalid_upstream());
    }
    let authorization_id = string_field(value, "authorizationId")?;
    let device_id = string_field(value, "deviceId")?;
    if !is_base64url_22(authorization_id)
        || !(16..=128).contains(&device_id.len())
        || device_id.chars().any(char::is_control)
        || !is_sha256_base64(string_field(value, "csrSpkiSha256")?)
        || !is_sha256_base64(string_field(value, "csrSha256")?)
        || value
            .get("authorizationGeneration")
            .and_then(Value::as_u64)
            .is_none_or(|generation| generation == 0)
    {
        return Err(invalid_upstream());
    }
    parse_datetime_ms(string_field(value, "expiresAt")?).map_err(|_| invalid_upstream())?;
    let scope = value.get("scope").ok_or_else(invalid_upstream)?;
    if !has_exact_keys(scope, &["organizationId", "workspaceId"])
        || string_field(scope, "organizationId")? != expected_scope.organization_id
        || string_field(scope, "workspaceId")? != expected_scope.workspace_id
    {
        return Err(invalid_upstream());
    }
    Ok(())
}

fn validate_identity(
    value: &Value,
    principal: &VerifiedWebPrincipal,
) -> Result<bool, ApprovalHttpError> {
    if !has_exact_keys(value, &["issuer", "subject"]) {
        return Ok(false);
    }
    Ok(
        string_field(value, "issuer")? == principal.identity().issuer
            && string_field(value, "subject")? == principal.identity().subject,
    )
}

fn validate_webauthn_options(value: &Value) -> Result<(), ApprovalHttpError> {
    let object = value.as_object().ok_or_else(invalid_upstream)?;
    const ALLOWED: &[&str] = &[
        "challenge",
        "rpId",
        "timeout",
        "allowCredentials",
        "userVerification",
        "hints",
        "extensions",
    ];
    if !object.contains_key("challenge")
        || object.keys().any(|key| !ALLOWED.contains(&key.as_str()))
    {
        return Err(invalid_upstream());
    }
    let challenge = string_field(value, "challenge")?;
    if challenge.is_empty()
        || challenge.len() > 1024
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(invalid_upstream());
    }
    if let Some(rp_id) = object.get("rpId") {
        let rp_id = rp_id.as_str().ok_or_else(invalid_upstream)?;
        if rp_id.is_empty() || rp_id.len() > 255 || rp_id.chars().any(char::is_control) {
            return Err(invalid_upstream());
        }
    }
    if let Some(timeout) = object.get("timeout") {
        if timeout.as_u64().is_none_or(|value| value == 0) {
            return Err(invalid_upstream());
        }
    }
    if let Some(allow_credentials) = object.get("allowCredentials") {
        let credentials = allow_credentials.as_array().ok_or_else(invalid_upstream)?;
        for credential in credentials {
            if !has_optional_keys(credential, &["type", "id"], &["transports"])
                || string_field(credential, "type")? != "public-key"
                || !is_base64url(string_field(credential, "id")?)
            {
                return Err(invalid_upstream());
            }
            if let Some(transports) = credential.get("transports") {
                let transports = transports.as_array().ok_or_else(invalid_upstream)?;
                if transports.iter().any(|transport| {
                    !transport.as_str().is_some_and(|value| {
                        matches!(
                            value,
                            "usb" | "nfc" | "ble" | "internal" | "hybrid" | "smart-card"
                        )
                    })
                }) {
                    return Err(invalid_upstream());
                }
            }
        }
    }
    if let Some(verification) = object.get("userVerification") {
        if !verification
            .as_str()
            .is_some_and(|value| matches!(value, "required" | "preferred" | "discouraged"))
        {
            return Err(invalid_upstream());
        }
    }
    if let Some(hints) = object.get("hints") {
        let hints = hints.as_array().ok_or_else(invalid_upstream)?;
        if hints.iter().any(|hint| {
            !hint
                .as_str()
                .is_some_and(|value| matches!(value, "security-key" | "client-device" | "hybrid"))
        }) {
            return Err(invalid_upstream());
        }
    }
    if object
        .get("extensions")
        .is_some_and(|value| !value.is_object())
    {
        return Err(invalid_upstream());
    }
    Ok(())
}

fn has_exact_keys(value: &Value, required: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == required.len() && required.iter().all(|key| object.contains_key(*key))
    })
}

fn has_optional_keys(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, ApprovalHttpError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(invalid_upstream)
}

fn parse_datetime_ms(value: &str) -> Result<u64, ApprovalHttpError> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid_upstream())?
        .timestamp_millis();
    u64::try_from(timestamp).map_err(|_| invalid_upstream())
}

fn is_base64url(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_base64url_22(value: &str) -> bool {
    value.len() == 22 && is_base64url(value)
}

fn is_sha256_base64(value: &str) -> bool {
    value.len() == 44
        && STANDARD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}

fn ceremony_id_from_approval_id(approval_id: &str) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(APPROVAL_ID_DOMAIN);
    digest.update((approval_id.len() as u64).to_be_bytes());
    digest.update(approval_id.as_bytes());
    let digest = digest.finalize();
    let mut ceremony_id = [0_u8; 16];
    ceremony_id.copy_from_slice(&digest[..16]);
    ceremony_id
}

fn ensure_response_size(value: &Value) -> Result<(), ApprovalHttpError> {
    let body = serde_json::to_vec(value).map_err(|_| invalid_upstream())?;
    if body.len() > MAX_JSON_BODY_BYTES {
        return Err(invalid_upstream());
    }
    Ok(())
}

fn map_binding_error(error: WebAuthnHttpSessionBindingError) -> ApprovalHttpError {
    match error {
        WebAuthnHttpSessionBindingError::NotFoundOrExpired => {
            ApprovalHttpError::new(ProblemCode::WorkspaceNotFound, "Not Found")
        }
        WebAuthnHttpSessionBindingError::Conflict => {
            ApprovalHttpError::new(ProblemCode::StateConflict, "Conflict")
        }
        WebAuthnHttpSessionBindingError::Unavailable => unavailable(),
    }
}

fn map_service_error(error: DeviceApprovalServiceError) -> ApprovalHttpError {
    match error {
        DeviceApprovalServiceError::InvalidRequest => {
            ApprovalHttpError::new(ProblemCode::InvalidRequest, "Bad Request")
        }
        DeviceApprovalServiceError::Forbidden => {
            ApprovalHttpError::new(ProblemCode::Forbidden, "Forbidden")
        }
        DeviceApprovalServiceError::NotFound => {
            ApprovalHttpError::new(ProblemCode::WorkspaceNotFound, "Not Found")
        }
        DeviceApprovalServiceError::Conflict => {
            ApprovalHttpError::new(ProblemCode::StateConflict, "Conflict")
        }
        DeviceApprovalServiceError::RateLimited => {
            ApprovalHttpError::new(ProblemCode::RateLimited, "Too Many Requests")
        }
        DeviceApprovalServiceError::Unavailable => unavailable(),
        DeviceApprovalServiceError::Internal => {
            ApprovalHttpError::new(ProblemCode::InternalError, "Internal Server Error")
        }
    }
}

fn invalid_upstream() -> ApprovalHttpError {
    ApprovalHttpError::new(ProblemCode::InvalidUpstreamResponse, "Bad Gateway")
}

fn unavailable() -> ApprovalHttpError {
    ApprovalHttpError::new(ProblemCode::UpstreamUnavailable, "Service Unavailable")
}

fn json_response(status: StatusCode, value: Value) -> Response {
    let body = match serde_json::to_vec(&value) {
        Ok(body) if body.len() <= MAX_JSON_BODY_BYTES => body,
        _ => {
            return problem_response(
                StatusCode::BAD_GATEWAY,
                ProblemCode::InvalidUpstreamResponse,
                "Bad Gateway",
                None,
            )
        }
    };
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HttpHeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HttpHeaderValue::from_static("no-store"));
    if let Ok(length) = HeaderValue::from_str(&body.len().to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}

struct ApprovalHttpError {
    status: StatusCode,
    code: ProblemCode,
    title: &'static str,
}

impl ApprovalHttpError {
    fn new(code: ProblemCode, title: &'static str) -> Self {
        Self {
            status: code.status(),
            code,
            title,
        }
    }

    fn with_status(status: StatusCode, code: ProblemCode, title: &'static str) -> Self {
        Self {
            status,
            code,
            title,
        }
    }
}

impl IntoResponse for ApprovalHttpError {
    fn into_response(self) -> Response {
        problem_response(self.status, self.code, self.title, None)
    }
}
