// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/http.rs             ║
// ║ Module: cy_workspace_web_bff::http                                 ║
// ║ Role: Serve the versioned same-origin Web BFF contract.             ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::http                                   ║
// ║ 职责：实现版本化同源 Web BFF HTTP 合同。                              ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::extract::{Path, State};
use axum::middleware::{self, Next};
use axum::routing::any;
use axum::Router;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use cy_workspace_control_plane::workspace_v1::WorkspaceConnectionDescriptor;
use cy_workspace_control_plane::{
    validate_descriptor, VerifiedWebPrincipal, WebIdentityError, WebPrincipalVerifier,
    WorkspaceDirectory, WorkspaceDirectoryError,
};
use cy_workspace_product_contracts::{
    parse_json_bytes_with_limit, JsonScopeBinding, MatchContextField, ProductOperation,
};
use http::header::{
    ACCEPT, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::csrf::{csrf_set_cookie, CsrfPrincipalBinding, CsrfSigner};
use crate::device_approval::{device_approval_router, DeviceApprovalDependencies};
use crate::problem::{problem_response, ProblemCode};
use crate::product::{
    product_response, workspace_product_request, ProductOperationCatalog, WorkspaceGatewayError,
    WorkspaceProductGateway, WorkspaceProductRequestInput,
};

/// Maximum UTF-8 JSON request or response body size, in bytes.
pub const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_INVOCATION_ENVELOPE_BYTES: usize = 6 * 1024 * 1024;
const MAX_ACCEPT_HEADER_BYTES: usize = 512;
const UNTRUSTED_EASY_AUTH_HEADER_PREFIX: &str = "x-ms-token-";
const TRACEPARENT_HEADER: &str = "traceparent";
const CSRF_HEADER: &str = "x-csrf-token";

/// Required, immutable BFF deployment configuration.
///
/// BFF 部署必须注入的不可变配置。
pub struct WebBffConfig {
    client_origin: String,
    csrf_signer: CsrfSigner,
}

impl WebBffConfig {
    /// Create configuration from the exact public Client origin and a runtime secret.
    ///
    /// 使用 Client 的精确公开 origin 与 runtime secret 创建配置。
    pub fn new(
        client_origin: impl Into<String>,
        csrf_mac_key: [u8; 32],
    ) -> Result<Self, WebBffStartupError> {
        let client_origin = client_origin.into();
        let parsed = url::Url::parse(&client_origin).map_err(|_| WebBffStartupError::Origin)?;
        if parsed.scheme() != "https"
            || parsed.origin().ascii_serialization() != client_origin
            || parsed.username() != ""
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(WebBffStartupError::Origin);
        }
        if csrf_mac_key.iter().all(|byte| *byte == 0) {
            return Err(WebBffStartupError::CsrfSecret);
        }
        Ok(Self {
            client_origin,
            csrf_signer: CsrfSigner::new(csrf_mac_key),
        })
    }
}

impl std::fmt::Debug for WebBffConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebBffConfig")
            .field("client_origin", &self.client_origin)
            .field("csrf_signer", &self.csrf_signer)
            .finish()
    }
}

/// Router startup failure. Missing production providers are never replaced with a stub.
///
/// Router 启动失败。生产 provider 缺失时绝不替换成 stub。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WebBffStartupError {
    /// The public Client origin is missing, malformed, or not an HTTPS origin.
    #[error("Client origin must be one exact HTTPS origin")]
    Origin,
    /// The injected CSRF signing key is absent or the all-zero placeholder.
    #[error("CSRF signing secret is not configured")]
    CsrfSecret,
}

/// Fully configured application state. All provider seams are required at construction.
///
/// 完整配置的应用状态；构造时必须提供所有 provider seam。
pub struct WebBffState {
    client_origin: String,
    csrf_signer: CsrfSigner,
    principal_verifier: Arc<dyn WebPrincipalVerifier>,
    directory: Arc<dyn WorkspaceDirectory>,
    workspace_api: Arc<dyn WorkspaceProductGateway>,
    product_operations: ProductOperationCatalog,
}

impl std::fmt::Debug for WebBffState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebBffState")
            .field("client_origin", &self.client_origin)
            .field("csrf_signer", &self.csrf_signer)
            .field("principal_verifier_configured", &true)
            .field("directory_configured", &true)
            .field("workspace_api_configured", &true)
            .field("product_catalog_configured", &true)
            .finish()
    }
}

impl WebBffState {
    /// Construct a router state only when every required runtime provider is present.
    ///
    /// 只有所有必要 runtime provider 均存在时才构造 router state。
    pub fn new(
        config: WebBffConfig,
        principal_verifier: Arc<dyn WebPrincipalVerifier>,
        directory: Arc<dyn WorkspaceDirectory>,
        workspace_api: Arc<dyn WorkspaceProductGateway>,
        product_operations: ProductOperationCatalog,
    ) -> Result<Self, WebBffStartupError> {
        Ok(Self {
            client_origin: config.client_origin,
            csrf_signer: config.csrf_signer,
            principal_verifier,
            directory,
            workspace_api,
            product_operations,
        })
    }
}

/// Build the versioned Web BFF router. No network listener or default provider is created.
///
/// 创建版本化 Web BFF router；不创建网络 listener 或默认 provider。
pub fn router(state: Arc<WebBffState>) -> Router {
    let device_approval = DeviceApprovalDependencies {
        service: None,
        directory: Some(state.directory.clone()),
        session_bindings: None,
    };
    router_with_device_approval(state, device_approval)
}

/// Build the BFF router with the three browser device-approval routes.
///
/// The caller supplies only trusted server-side providers. The current default
/// composition leaves absent providers unset, so those routes fail closed with
/// 503 until durable Device Authorization and WebAuthn adapters are configured.
///
/// 使用可信 server-side provider 构造含三个浏览器设备审批路由的 BFF router。
/// 缺失 provider 时路由固定 fail closed 为 503。
pub fn router_with_device_approval(
    state: Arc<WebBffState>,
    device_approval: DeviceApprovalDependencies,
) -> Router {
    let core_routes = Router::new()
        .route("/api/workspace/v1/session", any(session_route))
        .route("/api/workspace/v1/workspaces", any(workspaces_route))
        .route(
            "/api/workspace/v2/workspaces/:workspace_id/products/invocations",
            any(product_route),
        )
        .with_state(state.clone());
    let approval_routes = device_approval_router(device_approval).route_layer(
        middleware::from_fn_with_state(state.clone(), verified_web_session),
    );

    Router::new()
        .merge(core_routes)
        .merge(approval_routes)
        .fallback(not_found_route)
}

/// Apply the BFF's verified browser-session boundary to an externally composed router.
///
/// This middleware validates one Bearer identity, exact configured Origin, and
/// the signed CSRF header/cookie pair before inserting `VerifiedWebSessionContext`.
/// Use it for browser-facing credential ceremonies; keep workload-authenticated
/// Connector routes on their own transport boundary.
///
/// 为外部组合的 router 套用 BFF 已验证浏览器 session 边界。中间件会验证 Bearer identity、精确 Origin 与签名 CSRF
/// header/cookie，之后才注入 `VerifiedWebSessionContext`。该方法用于浏览器凭据 ceremony；Connector workload 路由应使用独立传输边界。
pub fn with_verified_web_session_routes(state: Arc<WebBffState>, routes: Router) -> Router {
    routes.route_layer(middleware::from_fn_with_state(state, verified_web_session))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSession<'a> {
    issuer: &'a str,
    subject: &'a str,
    organization_id: &'a str,
    expires_at: String,
    csrf_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceList {
    workspaces: Vec<WorkspaceSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSummary {
    workspace_id: String,
    organization_id: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProductInvocationEnvelope {
    owner_id: String,
    operation_id: String,
    json_body: Option<String>,
    resource_id: Option<String>,
    idempotency_key: Option<String>,
}

struct TraceInfo {
    traceparent: String,
    trace_id: String,
}

struct AuthenticatedRequest {
    principal: VerifiedWebPrincipal,
    access_token: String,
}

/// Authenticate one browser route and insert its typed session context.
///
/// This middleware runs only on routes explicitly wrapped by the BFF. It verifies the exact
/// configured Origin, bearer identity, and matching signed CSRF header/cookie
/// before inserting the context; raw credential headers are then removed before
/// the route handler runs.
///
/// 仅对 BFF 显式保护的路由验证精确 Origin、Bearer identity 与签名 CSRF header/cookie，成功后注入 typed context，
/// 并在进入 handler 前移除原始 credential header。
async fn verified_web_session(
    State(state): State<Arc<WebBffState>>,
    mut request: Request<Body>,
    next: Next,
) -> http::Response<Body> {
    let trace = match trace_info(request.headers()) {
        Ok(value) => value,
        Err(()) => return invalid_request(None),
    };
    if !exact_origin_matches(request.headers(), &state.client_origin) {
        return forbidden(Some(&trace.trace_id));
    }
    let authenticated = match authenticate(&state, request.headers()).await {
        Ok(value) => value,
        Err((status, code, title)) => {
            return problem_response(status, code, title, Some(&trace.trace_id));
        }
    };
    let csrf_header = match single_header(request.headers(), CSRF_HEADER) {
        Ok(value) => value.to_owned(),
        Err(()) => return csrf_failed(Some(&trace.trace_id)),
    };
    let csrf_cookie = match csrf_cookie_from_header(request.headers()) {
        Ok(value) => value.to_owned(),
        Err(()) => return csrf_failed(Some(&trace.trace_id)),
    };
    if csrf_header != csrf_cookie {
        return csrf_failed(Some(&trace.trace_id));
    }
    let now = match now_unix_ms() {
        Some(value) => value,
        None => return internal_error(Some(&trace.trace_id)),
    };
    let session = match state.csrf_signer.context_after_verified_csrf(
        authenticated.principal,
        &authenticated.access_token,
        &csrf_header,
        now,
    ) {
        Ok(value) => value,
        Err(_) => return csrf_failed(Some(&trace.trace_id)),
    };

    request.headers_mut().remove(http::header::AUTHORIZATION);
    request.headers_mut().remove(COOKIE);
    request.headers_mut().remove(CSRF_HEADER);
    request.extensions_mut().insert(session);
    next.run(request).await
}

async fn session_route(
    State(state): State<Arc<WebBffState>>,
    request: Request<Body>,
) -> http::Response<Body> {
    if request.method() != Method::GET {
        return method_not_allowed(None);
    }
    let trace = match trace_info(request.headers()) {
        Ok(value) => value,
        Err(()) => return invalid_request(None),
    };
    if request.uri().query().is_some() {
        return invalid_request(Some(&trace.trace_id));
    }
    if !accepts_json(request.headers()) {
        return not_acceptable(Some(&trace.trace_id));
    }
    let headers = request.headers().clone();
    if let Err(failure) = read_empty_get_body(request).await {
        return failure.response(Some(&trace.trace_id));
    }
    let authenticated = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err((status, code, title)) => {
            return problem_response(status, code, title, Some(&trace.trace_id));
        }
    };
    let now = match now_unix_ms() {
        Some(value) => value,
        None => return internal_error(Some(&trace.trace_id)),
    };
    let csrf = match state.csrf_signer.issue(
        CsrfPrincipalBinding::from_verified(&authenticated.principal),
        &authenticated.access_token,
        now,
    ) {
        Ok(value) => value,
        Err(_) => return unauthenticated(Some(&trace.trace_id)),
    };
    let expires_at = match DateTime::<Utc>::from_timestamp_millis(
        authenticated.principal.expires_at_unix_ms(),
    ) {
        Some(value) => value.to_rfc3339_opts(SecondsFormat::Millis, true),
        None => return unauthenticated(Some(&trace.trace_id)),
    };
    let body = WebSession {
        issuer: &authenticated.principal.identity().issuer,
        subject: &authenticated.principal.identity().subject,
        organization_id: authenticated.principal.organization_id(),
        expires_at,
        csrf_token: csrf.value.clone(),
    };
    let mut response = json_response(StatusCode::OK, &body, Some(&trace.trace_id));
    if let Ok(value) = HeaderValue::from_str(&csrf_set_cookie(&csrf, now)) {
        response.headers_mut().insert(SET_COOKIE, value);
    } else {
        return internal_error(Some(&trace.trace_id));
    }
    response
}

async fn workspaces_route(
    State(state): State<Arc<WebBffState>>,
    request: Request<Body>,
) -> http::Response<Body> {
    if request.method() != Method::GET {
        return method_not_allowed(None);
    }
    let trace = match trace_info(request.headers()) {
        Ok(value) => value,
        Err(()) => return invalid_request(None),
    };
    if request.uri().query().is_some() {
        return invalid_request(Some(&trace.trace_id));
    }
    if !accepts_json(request.headers()) {
        return not_acceptable(Some(&trace.trace_id));
    }
    let headers = request.headers().clone();
    if let Err(failure) = read_empty_get_body(request).await {
        return failure.response(Some(&trace.trace_id));
    }
    let authenticated = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err((status, code, title)) => {
            return problem_response(status, code, title, Some(&trace.trace_id));
        }
    };
    let now = match now_unix_ms() {
        Some(value) => value,
        None => return internal_error(Some(&trace.trace_id)),
    };
    let descriptors = match state
        .directory
        .discover(
            authenticated.principal.identity(),
            authenticated.principal.organization_id(),
            now.max(0) as u64,
        )
        .await
    {
        Ok(value) => value,
        Err(_) => return upstream_unavailable(Some(&trace.trace_id)),
    };
    let summaries = match project_member_workspaces(
        &state.directory,
        &authenticated.principal,
        descriptors,
        now.max(0) as u64,
    )
    .await
    {
        Ok(value) => value,
        Err(_) => return upstream_unavailable(Some(&trace.trace_id)),
    };
    let body = WorkspaceList {
        workspaces: summaries,
    };
    match serialize_json(&body) {
        Some(value) if value.len() <= MAX_JSON_BODY_BYTES => {
            raw_json_response(StatusCode::OK, value, Some(&trace.trace_id))
        }
        _ => problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ProblemCode::UpstreamUnavailable,
            "Service Unavailable",
            Some(&trace.trace_id),
        ),
    }
}

async fn product_route(
    State(state): State<Arc<WebBffState>>,
    Path(workspace_id): Path<String>,
    request: Request<Body>,
) -> http::Response<Body> {
    if request.method() != Method::POST {
        return method_not_allowed(None);
    }
    let trace = match trace_info(request.headers()) {
        Ok(value) => value,
        Err(()) => return invalid_request(None),
    };
    if !accepts_json(request.headers()) {
        return not_acceptable(Some(&trace.trace_id));
    }
    if request.uri().query().is_some() {
        return invalid_request(Some(&trace.trace_id));
    }
    if !exact_origin_matches(request.headers(), &state.client_origin) {
        return forbidden(Some(&trace.trace_id));
    }
    if workspace_id.is_empty() || workspace_id.len() > 200 {
        return invalid_request(Some(&trace.trace_id));
    }
    let authenticated = match authenticate(&state, request.headers()).await {
        Ok(value) => value,
        Err((status, code, title)) => {
            return problem_response(status, code, title, Some(&trace.trace_id));
        }
    };
    let now = match now_unix_ms() {
        Some(value) => value,
        None => return internal_error(Some(&trace.trace_id)),
    };
    let csrf_header = match single_header(request.headers(), CSRF_HEADER) {
        Ok(value) => value,
        Err(()) => return csrf_failed(Some(&trace.trace_id)),
    };
    let csrf_cookie = match csrf_cookie_from_header(request.headers()) {
        Ok(value) => value,
        Err(()) => return csrf_failed(Some(&trace.trace_id)),
    };
    if csrf_header != csrf_cookie
        || state
            .csrf_signer
            .verify(
                csrf_header,
                CsrfPrincipalBinding::from_verified(&authenticated.principal),
                &authenticated.access_token,
                now,
            )
            .is_err()
    {
        return csrf_failed(Some(&trace.trace_id));
    }

    let envelope = match read_product_invocation_envelope(request).await {
        Ok(value) => value,
        Err(failure) => return failure.response(Some(&trace.trace_id)),
    };
    if !is_valid_owner_id(&envelope.owner_id) || !is_valid_operation_id(&envelope.operation_id) {
        return invalid_request(Some(&trace.trace_id));
    }
    let Some(contract) = state
        .product_operations
        .get(&envelope.owner_id, &envelope.operation_id)
    else {
        return problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ProblemCode::UnsupportedOperation,
            "Unprocessable Content",
            Some(&trace.trace_id),
        );
    };
    if !state
        .product_operations
        .has_grant(&envelope.owner_id, &envelope.operation_id)
    {
        return forbidden(Some(&trace.trace_id));
    }
    let json_body = match decode_product_body(envelope.json_body.as_deref()) {
        Ok(value) => value,
        Err(failure) => return failure.response(Some(&trace.trace_id)),
    };
    let resource_id = match validate_resource_id(contract, envelope.resource_id.as_deref()) {
        Ok(value) => value,
        Err(()) => return invalid_request(Some(&trace.trace_id)),
    };
    let idempotency_key =
        match validate_idempotency_key(contract, envelope.idempotency_key.as_deref()) {
            Ok(value) => value,
            Err(()) => return invalid_request(Some(&trace.trace_id)),
        };
    let request_value = match contract.validate_request(json_body.as_deref()) {
        Ok(value) => value,
        Err(_) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::UnsupportedOperation,
                "Unprocessable Content",
                Some(&trace.trace_id),
            );
        }
    };
    if validate_scope_bindings(
        &contract.scope_bindings().request_bindings,
        request_value.as_ref(),
        authenticated.principal.organization_id(),
        &workspace_id,
        resource_id.as_deref(),
    )
    .is_err()
    {
        return forbidden(Some(&trace.trace_id));
    }
    let descriptors = match state
        .directory
        .discover(
            authenticated.principal.identity(),
            authenticated.principal.organization_id(),
            now.max(0) as u64,
        )
        .await
    {
        Ok(value) => value,
        Err(_) => return upstream_unavailable(Some(&trace.trace_id)),
    };
    match contains_member_workspace(
        &state.directory,
        &authenticated.principal,
        &descriptors,
        &workspace_id,
        now.max(0) as u64,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return workspace_not_found(Some(&trace.trace_id)),
        Err(_) => return upstream_unavailable(Some(&trace.trace_id)),
    }
    let request_id = Uuid::new_v4().to_string();
    let workspace_request = workspace_product_request(WorkspaceProductRequestInput {
        owner_id: &envelope.owner_id,
        operation_id: &envelope.operation_id,
        workspace_id: &workspace_id,
        resource_id: resource_id.as_deref(),
        json_body: json_body.as_deref(),
        idempotency_key: idempotency_key.as_deref(),
        traceparent: &trace.traceparent,
        request_id: &request_id,
    });
    let upstream = match state
        .workspace_api
        .invoke(&authenticated.principal, workspace_request)
        .await
    {
        Ok(value) => value,
        Err(error) => return gateway_error(error, Some(&trace.trace_id)),
    };
    let (status, content_type, response_body) = match product_response(upstream, &request_id) {
        Ok(value) => value,
        Err(error) => return gateway_error(error, Some(&trace.trace_id)),
    };
    if response_body.len() > MAX_JSON_BODY_BYTES {
        return invalid_upstream_response(Some(&trace.trace_id));
    }
    let response_value = match contract.validate_response(status, content_type, &response_body) {
        Ok(value) => value,
        Err(_) => return invalid_upstream_response(Some(&trace.trace_id)),
    };
    if validate_scope_bindings(
        &contract.scope_bindings().response_bindings,
        response_value.as_ref(),
        authenticated.principal.organization_id(),
        &workspace_id,
        resource_id.as_deref(),
    )
    .is_err()
    {
        return invalid_upstream_response(Some(&trace.trace_id));
    }
    if status >= 400 {
        let Some(value) = response_value.as_ref() else {
            return invalid_upstream_response(Some(&trace.trace_id));
        };
        if (content_type == "application/problem+json"
            && validate_public_problem_details(status, value).is_err())
            || contains_unsafe_error_address(value)
        {
            return invalid_upstream_response(Some(&trace.trace_id));
        }
    }
    raw_product_response(status, content_type, response_body, Some(&trace.trace_id))
}

async fn not_found_route() -> http::Response<Body> {
    problem_response(
        StatusCode::NOT_FOUND,
        ProblemCode::WorkspaceNotFound,
        "Not Found",
        None,
    )
}

async fn read_product_invocation_envelope(
    request: Request<Body>,
) -> Result<ProductInvocationEnvelope, RequestFailure> {
    let announced_length = content_length(request.headers())?;
    if announced_length.is_some_and(|length| length > MAX_INVOCATION_ENVELOPE_BYTES) {
        return Err(RequestFailure::PayloadTooLarge);
    }
    if !is_json_content_type(request.headers()) {
        return Err(RequestFailure::UnsupportedMediaType);
    }
    let bytes = to_bytes(request.into_body(), MAX_INVOCATION_ENVELOPE_BYTES)
        .await
        .map_err(|_| RequestFailure::PayloadTooLarge)?;
    if bytes.is_empty() {
        return Err(RequestFailure::InvalidJson);
    }
    parse_json_bytes_with_limit(&bytes, MAX_INVOCATION_ENVELOPE_BYTES)
        .map_err(|_| RequestFailure::InvalidJson)?;
    serde_json::from_slice(&bytes).map_err(|_| RequestFailure::InvalidJson)
}

fn decode_product_body(encoded: Option<&str>) -> Result<Option<Vec<u8>>, RequestFailure> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| RequestFailure::InvalidJson)?;
    if BASE64_STANDARD.encode(&bytes) != encoded {
        return Err(RequestFailure::InvalidJson);
    }
    if bytes.len() > MAX_JSON_BODY_BYTES {
        return Err(RequestFailure::PayloadTooLarge);
    }
    if bytes.is_empty() {
        return Err(RequestFailure::InvalidJson);
    }
    Ok(Some(bytes))
}

fn is_valid_owner_id(value: &str) -> bool {
    let mut characters = value.bytes();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && value.len() <= 63
        && characters.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_valid_operation_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_resource_id(
    operation: &ProductOperation,
    value: Option<&str>,
) -> Result<Option<String>, ()> {
    if value.is_some_and(|value| !is_safe_opaque_resource_id(value)) {
        return Err(());
    }
    operation.validate_resource_id(value).map_err(|_| ())?;
    Ok(value.map(str::to_owned))
}

fn validate_idempotency_key(
    operation: &ProductOperation,
    value: Option<&str>,
) -> Result<Option<String>, ()> {
    operation.validate_idempotency_key(value).map_err(|_| ())?;
    Ok(value.map(str::to_owned))
}

fn validate_scope_bindings(
    bindings: &[JsonScopeBinding],
    value: Option<&Value>,
    organization_id: &str,
    workspace_id: &str,
    resource_id: Option<&str>,
) -> Result<(), ()> {
    if bindings.is_empty() {
        return Ok(());
    }
    let value = value.ok_or(())?;
    for binding in bindings {
        let expected = match binding.matches {
            MatchContextField::OrganizationId => organization_id,
            MatchContextField::WorkspaceId => workspace_id,
            MatchContextField::ResourceId => resource_id.ok_or(())?,
        };
        let selected = json_pointer_values(value, &binding.json_pointer)?;
        if selected.is_empty()
            || selected
                .iter()
                .any(|candidate| candidate.as_str() != Some(expected))
        {
            return Err(());
        }
    }
    Ok(())
}

fn json_pointer_values<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a Value>, ()> {
    if pointer.len() > 1024 || !pointer.starts_with('/') {
        return Err(());
    }
    let segments = pointer
        .split('/')
        .skip(1)
        .map(unescape_json_pointer_segment)
        .collect::<Result<Vec<_>, _>>()?;
    if segments.len() > 64 {
        return Err(());
    }
    let mut current = vec![value];
    for segment in segments {
        let mut next = Vec::new();
        for selected in current {
            match selected {
                Value::Array(items) if segment == "*" => next.extend(items.iter()),
                Value::Object(items) if segment == "*" => next.extend(items.values()),
                Value::Array(items) => {
                    let index = segment.parse::<usize>().map_err(|_| ())?;
                    if let Some(item) = items.get(index) {
                        next.push(item);
                    }
                }
                Value::Object(items) => {
                    if let Some(item) = items.get(&segment) {
                        next.push(item);
                    }
                }
                _ => {}
            }
            if next.len() > 16_384 {
                return Err(());
            }
        }
        current = next;
        if current.is_empty() {
            return Err(());
        }
    }
    Ok(current)
}

fn unescape_json_pointer_segment(segment: &str) -> Result<String, ()> {
    let mut result = String::with_capacity(segment.len());
    let mut characters = segment.chars();
    while let Some(character) = characters.next() {
        if character != '~' {
            result.push(character);
            continue;
        }
        match characters.next().ok_or(())? {
            '0' => result.push('~'),
            '1' => result.push('/'),
            _ => return Err(()),
        }
    }
    Ok(result)
}

fn is_safe_opaque_resource_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value != "."
        && value != ".."
        && !value.contains("..")
        && value.bytes().all(|byte| {
            !byte.is_ascii_control() && !matches!(byte, b'/' | b'\\' | b'%' | b'?' | b'#')
        })
}

fn validate_public_problem_details(status: u16, value: &Value) -> Result<(), ()> {
    let object = value.as_object().ok_or(())?;
    let allowed = [
        "type",
        "title",
        "status",
        "detail",
        "instance",
        "code",
        "retryable",
        "traceId",
        "resourceRef",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || object.contains_key("resourceRef")
        || object.get("type").and_then(Value::as_str) != Some("about:blank")
        || object.get("instance").and_then(Value::as_str) != Some("about:blank")
        || object.get("status").and_then(Value::as_u64) != Some(u64::from(status))
    {
        return Err(());
    }
    Ok(())
}

fn contains_unsafe_error_address(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            let normalized = text.to_ascii_lowercase();
            text.contains('/')
                || text.contains('\\')
                || normalized.contains("://")
                || normalized.contains(".internal")
                || normalized.contains(".svc")
                || normalized.contains(".cluster.local")
                || normalized.contains("localhost")
                || contains_private_ipv4_address(text)
        }
        Value::Array(items) => items.iter().any(contains_unsafe_error_address),
        Value::Object(object) => object.values().any(contains_unsafe_error_address),
        _ => false,
    }
}

fn contains_private_ipv4_address(text: &str) -> bool {
    text.split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|candidate| candidate.contains('.'))
        .filter_map(|candidate| {
            let octets = candidate
                .split('.')
                .map(str::parse::<u8>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            (octets.len() == 4).then_some(octets)
        })
        .any(|octets| {
            octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 127)
                || (octets[0] == 169 && octets[1] == 254)
        })
}

async fn read_empty_get_body(request: Request<Body>) -> Result<(), RequestFailure> {
    let content_length = content_length(request.headers())?;
    if content_length.is_some_and(|length| length > MAX_JSON_BODY_BYTES) {
        return Err(RequestFailure::PayloadTooLarge);
    }
    let bytes = to_bytes(request.into_body(), MAX_JSON_BODY_BYTES)
        .await
        .map_err(|_| RequestFailure::PayloadTooLarge)?;
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(RequestFailure::InvalidJson)
    }
}

#[derive(Debug)]
enum RequestFailure {
    InvalidJson,
    UnsupportedMediaType,
    PayloadTooLarge,
}

impl RequestFailure {
    fn response(&self, trace_id: Option<&str>) -> http::Response<Body> {
        match self {
            Self::InvalidJson => invalid_request(trace_id),
            Self::UnsupportedMediaType => problem_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ProblemCode::UnsupportedMediaType,
                "Unsupported Media Type",
                trace_id,
            ),
            Self::PayloadTooLarge => problem_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                ProblemCode::PayloadTooLarge,
                "Content Too Large",
                trace_id,
            ),
        }
    }
}

async fn project_member_workspaces(
    directory: &Arc<dyn WorkspaceDirectory>,
    principal: &VerifiedWebPrincipal,
    descriptors: Vec<WorkspaceConnectionDescriptor>,
    now_unix_ms: u64,
) -> Result<Vec<WorkspaceSummary>, WorkspaceDirectoryError> {
    let mut seen = BTreeSet::new();
    let mut summaries = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        validate_descriptor(descriptor, now_unix_ms)?;
        if descriptor.organization_id != principal.organization_id()
            || !seen.insert(descriptor.workspace_id.as_str())
            || descriptor.workspace_id.len() > 200
            || descriptor.organization_id.len() > 200
            || descriptor.display_name.len() > 256
        {
            return Err(WorkspaceDirectoryError::Descriptor(
                "member summary is inconsistent".to_owned(),
            ));
        }
        if !directory
            .is_member(
                principal.identity(),
                principal.organization_id(),
                &descriptor.workspace_id,
            )
            .await?
        {
            return Err(WorkspaceDirectoryError::Descriptor(
                "member summary is inconsistent".to_owned(),
            ));
        }
        summaries.push(WorkspaceSummary {
            workspace_id: descriptor.workspace_id.clone(),
            organization_id: descriptor.organization_id.clone(),
            display_name: descriptor.display_name.clone(),
        });
    }
    Ok(summaries)
}

async fn contains_member_workspace(
    directory: &Arc<dyn WorkspaceDirectory>,
    principal: &VerifiedWebPrincipal,
    descriptors: &[WorkspaceConnectionDescriptor],
    workspace_id: &str,
    now_unix_ms: u64,
) -> Result<bool, WorkspaceDirectoryError> {
    for descriptor in descriptors {
        if validate_descriptor(descriptor, now_unix_ms).is_ok()
            && descriptor.workspace_id == workspace_id
            && descriptor.organization_id == principal.organization_id()
            && directory
                .is_member(
                    principal.identity(),
                    principal.organization_id(),
                    workspace_id,
                )
                .await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn authenticate(
    state: &WebBffState,
    headers: &HeaderMap,
) -> Result<AuthenticatedRequest, (StatusCode, ProblemCode, &'static str)> {
    let token = bearer_access_token(headers).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            ProblemCode::Unauthenticated,
            "Unauthorized",
        )
    })?;
    let principal = state
        .principal_verifier
        .verify_access_token(token)
        .await
        .map_err(identity_failure)?;
    if principal.identity().issuer.is_empty()
        || principal.identity().issuer.len() > 2048
        || url::Url::parse(&principal.identity().issuer).is_err()
        || principal.identity().subject.is_empty()
        || principal.identity().subject.len() > 512
        || principal.organization_id().is_empty()
        || principal.organization_id().len() > 200
        || principal.expires_at_unix_ms() <= now_unix_ms().unwrap_or_default()
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            ProblemCode::InvalidPrincipal,
            "Unauthorized",
        ));
    }
    Ok(AuthenticatedRequest {
        principal,
        access_token: token.to_owned(),
    })
}

fn bearer_access_token(headers: &HeaderMap) -> Result<&str, ()> {
    if headers
        .keys()
        .any(|name| name.as_str().starts_with(UNTRUSTED_EASY_AUTH_HEADER_PREFIX))
    {
        return Err(());
    }
    let authorization = single_header(headers, http::header::AUTHORIZATION.as_str())?;
    let (scheme, token) = authorization.split_once(' ').ok_or(())?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.len() > 16 * 1024
        || token.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(());
    }
    Ok(token)
}

fn identity_failure(error: WebIdentityError) -> (StatusCode, ProblemCode, &'static str) {
    match error.http_status() {
        401 => (
            StatusCode::UNAUTHORIZED,
            ProblemCode::Unauthenticated,
            "Unauthorized",
        ),
        403 => (StatusCode::FORBIDDEN, ProblemCode::Forbidden, "Forbidden"),
        503 => (
            StatusCode::SERVICE_UNAVAILABLE,
            ProblemCode::UpstreamUnavailable,
            "Service Unavailable",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ProblemCode::InternalError,
            "Internal Server Error",
        ),
    }
}

fn exact_origin_matches(headers: &HeaderMap, configured_origin: &str) -> bool {
    single_header(headers, ORIGIN.as_str()).is_ok_and(|origin| origin == configured_origin)
}

fn csrf_cookie_from_header(headers: &HeaderMap) -> Result<&str, ()> {
    let cookie = single_header(headers, COOKIE.as_str())?;
    let mut csrf_value = None;
    for part in cookie.split(';') {
        let (name, value) = part.trim().split_once('=').ok_or(())?;
        if name == crate::csrf::csrf_cookie_name() {
            if csrf_value.is_some() || value.is_empty() {
                return Err(());
            }
            csrf_value = Some(value);
        }
    }
    csrf_value.ok_or(())
}

fn trace_info(headers: &HeaderMap) -> Result<TraceInfo, ()> {
    let supplied = match single_header(headers, TRACEPARENT_HEADER) {
        Ok(value) => Some(value),
        Err(()) if !headers.contains_key(TRACEPARENT_HEADER) => None,
        Err(()) => return Err(()),
    };
    match supplied {
        Some(value) => parse_traceparent(value),
        None => {
            let trace_id = Uuid::new_v4().simple().to_string();
            let parent_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
            Ok(TraceInfo {
                traceparent: format!("00-{trace_id}-{parent_id}-01"),
                trace_id,
            })
        }
    }
}

fn parse_traceparent(value: &str) -> Result<TraceInfo, ()> {
    if value.len() != 55
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
    {
        return Err(());
    }
    let bytes = value.as_bytes();
    if &bytes[0..3] != b"00-"
        || bytes[35] != b'-'
        || bytes[52] != b'-'
        || bytes[3..35].iter().all(|byte| *byte == b'0')
        || bytes[36..52].iter().all(|byte| *byte == b'0')
    {
        return Err(());
    }
    Ok(TraceInfo {
        traceparent: value.to_owned(),
        trace_id: value[3..35].to_owned(),
    })
}

fn accepts_json(headers: &HeaderMap) -> bool {
    let values = headers.get_all(ACCEPT).iter().collect::<Vec<_>>();
    if values.is_empty() {
        return true;
    }
    let mut combined_length = 0;
    let mut ranges = Vec::new();
    for value in values {
        let Ok(value) = value.to_str() else {
            return false;
        };
        combined_length += value.len();
        if combined_length > MAX_ACCEPT_HEADER_BYTES {
            return false;
        }
        ranges.extend(value.split(','));
    }
    ranges.into_iter().any(|range| {
        let mut parts = range.trim().split(';');
        let media = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let mut quality = 1.0_f32;
        for parameter in parts {
            if let Some((name, value)) = parameter.trim().split_once('=') {
                if name.trim().eq_ignore_ascii_case("q") {
                    quality = value.trim().parse::<f32>().unwrap_or(0.0);
                }
            }
        }
        quality > 0.0
            && matches!(
                media.as_str(),
                "*/*" | "application/*" | "application/json" | "application/problem+json"
            )
    })
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Ok(value) = single_header(headers, CONTENT_TYPE.as_str()) else {
        return false;
    };
    let Ok(content_type) = value.parse::<mime::Mime>() else {
        return false;
    };
    if content_type.essence_str() != "application/json" {
        return false;
    }
    content_type.params().all(|(name, value)| {
        name.as_str().eq_ignore_ascii_case("charset")
            && value.as_str().eq_ignore_ascii_case("utf-8")
    })
}

fn content_length(headers: &HeaderMap) -> Result<Option<usize>, RequestFailure> {
    let values = headers.get_all(CONTENT_LENGTH).iter().collect::<Vec<_>>();
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 {
        return Err(RequestFailure::InvalidJson);
    }
    let value = values[0]
        .to_str()
        .map_err(|_| RequestFailure::InvalidJson)?
        .parse::<usize>()
        .map_err(|_| RequestFailure::InvalidJson)?;
    Ok(Some(value))
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ()> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| ())?;
    let values = headers.get_all(name).iter().collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(());
    }
    values[0].to_str().map_err(|_| ())
}

fn now_unix_ms() -> Option<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
}

fn serialize_json<T: Serialize>(value: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(value).ok()
}

fn json_response<T: Serialize>(
    status: StatusCode,
    value: &T,
    trace_id: Option<&str>,
) -> http::Response<Body> {
    match serialize_json(value) {
        Some(body) if body.len() <= MAX_JSON_BODY_BYTES => {
            raw_json_response(status, body, trace_id)
        }
        _ => internal_error(trace_id),
    }
}

fn raw_json_response(
    status: StatusCode,
    body: Vec<u8>,
    _trace_id: Option<&str>,
) -> http::Response<Body> {
    let mut response = http::Response::new(Body::from(body.clone()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    set_content_length(&mut response, body.len());
    response
}

fn raw_product_response(
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    _trace_id: Option<&str>,
) -> http::Response<Body> {
    let Ok(status) = StatusCode::from_u16(status) else {
        return invalid_upstream_response(None);
    };
    let mut response = http::Response::new(Body::from(body.clone()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    set_content_length(&mut response, body.len());
    response
}

fn set_content_length(response: &mut http::Response<Body>, length: usize) {
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, value);
    }
}

fn method_not_allowed(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::METHOD_NOT_ALLOWED,
        ProblemCode::InvalidRequest,
        "Method Not Allowed",
        trace_id,
    )
}

fn invalid_request(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::BAD_REQUEST,
        ProblemCode::InvalidRequest,
        "Bad Request",
        trace_id,
    )
}

fn not_acceptable(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::NOT_ACCEPTABLE,
        ProblemCode::InvalidRequest,
        "Not Acceptable",
        trace_id,
    )
}

fn unauthenticated(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::UNAUTHORIZED,
        ProblemCode::Unauthenticated,
        "Unauthorized",
        trace_id,
    )
}

fn forbidden(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::FORBIDDEN,
        ProblemCode::Forbidden,
        "Forbidden",
        trace_id,
    )
}

fn csrf_failed(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::FORBIDDEN,
        ProblemCode::CsrfFailed,
        "Forbidden",
        trace_id,
    )
}

fn workspace_not_found(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::NOT_FOUND,
        ProblemCode::WorkspaceNotFound,
        "Not Found",
        trace_id,
    )
}

fn internal_error(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        ProblemCode::InternalError,
        "Internal Server Error",
        trace_id,
    )
}

fn upstream_unavailable(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::SERVICE_UNAVAILABLE,
        ProblemCode::UpstreamUnavailable,
        "Service Unavailable",
        trace_id,
    )
}

fn invalid_upstream_response(trace_id: Option<&str>) -> http::Response<Body> {
    problem_response(
        StatusCode::BAD_GATEWAY,
        ProblemCode::InvalidUpstreamResponse,
        "Bad Gateway",
        trace_id,
    )
}

fn gateway_error(error: WorkspaceGatewayError, trace_id: Option<&str>) -> http::Response<Body> {
    match error {
        WorkspaceGatewayError::Unavailable => upstream_unavailable(trace_id),
        WorkspaceGatewayError::Timeout => problem_response(
            StatusCode::GATEWAY_TIMEOUT,
            ProblemCode::UpstreamTimeout,
            "Gateway Timeout",
            trace_id,
        ),
        WorkspaceGatewayError::Forbidden => forbidden(trace_id),
        WorkspaceGatewayError::NotFound => workspace_not_found(trace_id),
        WorkspaceGatewayError::InvalidResponse => invalid_upstream_response(trace_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use axum::body::to_bytes;
    use cy_workspace_control_plane::test_principal;
    use cy_workspace_control_plane::workspace_v1::{UserIdentityRef, WorkspaceApiRequest};
    use cy_workspace_control_plane::{
        VerifiedWebPrincipal, WebIdentityError, WorkspaceDirectoryError,
    };
    use http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use serde_json::Value;
    use tower::ServiceExt;

    struct RejectingVerifier {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WebPrincipalVerifier for RejectingVerifier {
        async fn verify_access_token(
            &self,
            _access_token: &str,
        ) -> Result<VerifiedWebPrincipal, WebIdentityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(WebIdentityError::InvalidToken)
        }
    }

    struct AcceptingVerifier {
        calls: Arc<AtomicUsize>,
        access_token: String,
        principal: VerifiedWebPrincipal,
    }

    #[async_trait]
    impl WebPrincipalVerifier for AcceptingVerifier {
        async fn verify_access_token(
            &self,
            access_token: &str,
        ) -> Result<VerifiedWebPrincipal, WebIdentityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if access_token == self.access_token {
                Ok(self.principal.clone())
            } else {
                Err(WebIdentityError::InvalidToken)
            }
        }
    }

    struct EmptyWorkspaceDirectory;

    #[async_trait]
    impl WorkspaceDirectory for EmptyWorkspaceDirectory {
        async fn discover(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            Ok(Vec::new())
        }

        async fn is_member(
            &self,
            _user: &UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(false)
        }
    }

    struct CountingGateway {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WorkspaceProductGateway for CountingGateway {
        async fn invoke(
            &self,
            _principal: &VerifiedWebPrincipal,
            _request: WorkspaceApiRequest,
        ) -> Result<
            cy_workspace_control_plane::workspace_v1::WorkspaceApiResponse,
            WorkspaceGatewayError,
        > {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(WorkspaceGatewayError::Unavailable)
        }
    }

    fn test_catalog() -> ProductOperationCatalog {
        ProductOperationCatalog::deny_all_for_tests()
    }

    fn test_state() -> (Arc<WebBffState>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let verifier_calls = Arc::new(AtomicUsize::new(0));
        let gateway_calls = Arc::new(AtomicUsize::new(0));
        let state = WebBffState::new(
            WebBffConfig::new("https://client.example", [0x5a; 32]).expect("valid config"),
            Arc::new(RejectingVerifier {
                calls: Arc::clone(&verifier_calls),
            }),
            Arc::new(EmptyWorkspaceDirectory),
            Arc::new(CountingGateway {
                calls: Arc::clone(&gateway_calls),
            }),
            test_catalog(),
        )
        .expect("all runtime ports and contracts are supplied");
        (Arc::new(state), verifier_calls, gateway_calls)
    }

    async fn assert_problem(
        response: http::Response<Body>,
        expected_status: StatusCode,
        expected_code: &str,
    ) {
        assert_eq!(response.status(), expected_status);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = to_bytes(response.into_body(), MAX_JSON_BODY_BYTES)
            .await
            .expect("problem body is bounded");
        let body: Value = serde_json::from_slice(&body).expect("problem body is JSON");
        assert_eq!(body["status"], expected_status.as_u16());
        assert_eq!(body["code"], expected_code);
    }

    #[test]
    fn traceparent_rejects_zero_ids_and_uppercase_hex() {
        assert!(
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_ok()
        );
        assert!(
            parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_err()
        );
        assert!(
            parse_traceparent("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01").is_err()
        );
    }

    #[test]
    fn origin_requires_one_exact_configured_value() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("https://client.example"));
        assert!(exact_origin_matches(&headers, "https://client.example"));
        assert!(!exact_origin_matches(&headers, "https://client.example/"));
    }

    #[test]
    fn one_well_formed_bearer_header_is_the_only_token_input() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );
        assert_eq!(bearer_access_token(&headers), Ok("opaque-token"));

        headers.insert(
            HeaderName::from_static("x-ms-token-aad-access-token"),
            HeaderValue::from_static("legacy-token"),
        );
        assert!(bearer_access_token(&headers).is_err());
    }

    #[test]
    fn v2_invocation_envelope_is_closed_and_uses_generic_ids() {
        let envelope: ProductInvocationEnvelope = serde_json::from_value(serde_json::json!({
            "ownerId": "echo",
            "operationId": "workspaceCreateEvaluationSuite",
            "jsonBody": "eyJuYW1lIjoiZXhhbXBsZSJ9",
            "resourceId": "suite-1",
            "idempotencyKey": "create-1"
        }))
        .expect("generic v2 invocation is valid");
        assert_eq!(envelope.owner_id, "echo");
        assert_eq!(envelope.operation_id, "workspaceCreateEvaluationSuite");
        assert!(
            serde_json::from_value::<ProductInvocationEnvelope>(serde_json::json!({
                "ownerId": "echo",
                "operationId": "workspaceCreateEvaluationSuite",
                "catalogVersion": "2.0.0"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProductInvocationEnvelope>(serde_json::json!({
                "ownerId": "echo",
                "operationId": "workspaceCreateEvaluationSuite",
                "url": "https://product.example"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn invocation_envelope_rejects_recursive_and_escaped_duplicate_keys() {
        let duplicate_payloads = [
            r#"{"ownerId":"echo","ownerId":"navigator","operationId":"observeWorkspaceSnapshot"}"#,
            r#"{"ownerId":"echo","\u006fwnerId":"navigator","operationId":"observeWorkspaceSnapshot"}"#,
            r#"{"ownerId":"echo","operationId":"observeWorkspaceSnapshot","extension":{"value":1,"value":2}}"#,
        ];
        for payload in duplicate_payloads {
            let request = Request::builder()
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .expect("request is valid");
            assert!(matches!(
                read_product_invocation_envelope(request).await,
                Err(RequestFailure::InvalidJson)
            ));
        }
    }

    #[tokio::test]
    async fn oversized_invocation_envelope_maps_to_payload_too_large() {
        let request = Request::builder()
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, MAX_INVOCATION_ENVELOPE_BYTES + 1)
            .body(Body::empty())
            .expect("request is valid");
        assert!(matches!(
            read_product_invocation_envelope(request).await,
            Err(RequestFailure::PayloadTooLarge)
        ));
    }

    #[tokio::test]
    async fn invocation_envelope_keeps_its_six_mib_limit() {
        let padding = " ".repeat(5 * 1024 * 1024);
        let body = format!(
            "{padding}{{\"ownerId\":\"echo\",\"operationId\":\"workspaceGetEvaluationSuite\"}}"
        );
        let request = Request::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("request is valid");

        let envelope = read_product_invocation_envelope(request)
            .await
            .expect("the envelope cap is independent from the four MiB Product body cap");
        assert_eq!(envelope.owner_id, "echo");
    }

    #[test]
    fn decoded_product_body_preserves_exact_bytes_and_enforces_four_mib() {
        let original = b"{ \"workspaceId\" : \"ws-1\" }\n";
        let encoded = BASE64_STANDARD.encode(original);
        assert_eq!(
            decode_product_body(Some(&encoded)).unwrap(),
            Some(original.to_vec())
        );

        let oversized = BASE64_STANDARD.encode(vec![b'x'; MAX_JSON_BODY_BYTES + 1]);
        assert!(matches!(
            decode_product_body(Some(&oversized)),
            Err(RequestFailure::PayloadTooLarge)
        ));
        assert!(decode_product_body(Some("not base64!")).is_err());
    }

    #[test]
    fn scope_bindings_reject_cross_workspace_values_and_check_every_wildcard_match() {
        let bindings = [JsonScopeBinding {
            json_pointer: "/workspaces/*/workspaceId".to_owned(),
            matches: MatchContextField::WorkspaceId,
        }];
        let valid = serde_json::json!({ "workspaces": [
            { "workspaceId": "ws-1" }, { "workspaceId": "ws-1" }
        ] });
        let foreign = serde_json::json!({ "workspaces": [
            { "workspaceId": "ws-1" }, { "workspaceId": "ws-2" }
        ] });
        let missing = serde_json::json!({ "workspaces": [] });
        assert!(validate_scope_bindings(&bindings, Some(&valid), "org-1", "ws-1", None).is_ok());
        assert!(validate_scope_bindings(&bindings, Some(&foreign), "org-1", "ws-1", None).is_err());
        assert!(validate_scope_bindings(&bindings, Some(&missing), "org-1", "ws-1", None).is_err());
        assert!(validate_scope_bindings(
            &bindings,
            Some(&serde_json::json!({})),
            "org-1",
            "ws-1",
            None
        )
        .is_err());
    }

    #[test]
    fn all_zero_csrf_secret_cannot_construct_router_configuration() {
        assert_eq!(
            WebBffConfig::new("https://client.example", [0; 32]).err(),
            Some(WebBffStartupError::CsrfSecret)
        );
    }

    #[tokio::test]
    async fn unsigned_easy_auth_principal_header_is_not_identity_evidence() {
        let (state, verifier_calls, gateway_calls) = test_state();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/workspace/v1/session")
            .header("x-ms-client-principal", "forged-user")
            .body(Body::empty())
            .expect("request is valid");
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");

        assert_problem(response, StatusCode::UNAUTHORIZED, "unauthenticated").await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn duplicate_authorization_headers_are_rejected_before_verification() {
        let (state, verifier_calls, gateway_calls) = test_state();
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/workspace/v1/session")
            .body(Body::empty())
            .expect("request is valid");
        request.headers_mut().append(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer first-token"),
        );
        request.headers_mut().append(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer second-token"),
        );
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");

        assert_problem(response, StatusCode::UNAUTHORIZED, "unauthenticated").await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_bearer_and_legacy_easy_auth_token_headers_are_rejected() {
        let cases = [
            ("Basic opaque-token", false),
            ("Bearer", false),
            ("Bearer token with-space", false),
            ("Bearer valid-shaped-token", true),
        ];
        for (authorization, add_legacy_header) in cases {
            let (state, verifier_calls, gateway_calls) = test_state();
            let mut request = Request::builder()
                .method(Method::GET)
                .uri("/api/workspace/v1/session")
                .header(http::header::AUTHORIZATION, authorization)
                .body(Body::empty())
                .expect("request is valid");
            if add_legacy_header {
                request.headers_mut().insert(
                    HeaderName::from_static("x-ms-token-aad-access-token"),
                    HeaderValue::from_static("legacy-token"),
                );
            }
            let response = router(state)
                .oneshot(request)
                .await
                .expect("router responds");

            assert_problem(response, StatusCode::UNAUTHORIZED, "unauthenticated").await;
            assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
            assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn v2_product_post_requires_exact_origin_before_authentication() {
        let (state, verifier_calls, gateway_calls) = test_state();
        for origin in [None, Some("https://attacker.example")] {
            let mut request = Request::builder()
                .method(Method::POST)
                .uri("/api/workspace/v2/workspaces/ws-1/products/invocations")
                .body(Body::empty())
                .expect("request is valid");
            if let Some(origin) = origin {
                request.headers_mut().insert(
                    ORIGIN,
                    HeaderValue::from_str(origin).expect("origin is a header value"),
                );
            }
            let response = router(Arc::clone(&state))
                .oneshot(request)
                .await
                .expect("router responds");
            assert_problem(response, StatusCode::FORBIDDEN, "forbidden").await;
        }
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn v2_product_post_accepts_the_session_cookie_origin_and_signed_double_submit_token() {
        let verifier_calls = Arc::new(AtomicUsize::new(0));
        let gateway_calls = Arc::new(AtomicUsize::new(0));
        let access_token = "test-access-token-bound-to-csrf-session".to_owned();
        let now = now_unix_ms().expect("system clock is available");
        let principal = test_principal(
            UserIdentityRef {
                issuer: "https://login.example/tenant/v2.0".to_owned(),
                subject: "web-user-1".to_owned(),
            },
            "org-1",
            now + 60_000,
        );
        let state = Arc::new(
            WebBffState::new(
                WebBffConfig::new("https://client.example", [0x5a; 32]).expect("valid config"),
                Arc::new(AcceptingVerifier {
                    calls: Arc::clone(&verifier_calls),
                    access_token: access_token.clone(),
                    principal: principal.clone(),
                }),
                Arc::new(EmptyWorkspaceDirectory),
                Arc::new(CountingGateway {
                    calls: Arc::clone(&gateway_calls),
                }),
                test_catalog(),
            )
            .expect("all runtime ports and contracts are supplied"),
        );
        let issued = state
            .csrf_signer
            .issue(
                CsrfPrincipalBinding::from_verified(&principal),
                &access_token,
                now,
            )
            .expect("verified session receives a signed CSRF token");
        let set_cookie = csrf_set_cookie(&issued, now);
        assert!(set_cookie.contains("Path=/api/workspace;"));
        assert!(set_cookie.contains("Secure; HttpOnly; SameSite=Strict"));
        assert!(!set_cookie.contains("Domain="));
        let cookie_pair = set_cookie.split(';').next().expect("cookie has a pair");

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/workspace/v2/workspaces/ws-1/products/invocations")
            .header(ORIGIN, "https://client.example")
            .header(
                http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header(COOKIE, cookie_pair)
            .header(CSRF_HEADER, issued.value)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(Body::from(
                r#"{"ownerId":"unlisted","operationId":"unknown"}"#,
            ))
            .expect("request is valid");
        let response = router(Arc::clone(&state))
            .oneshot(request)
            .await
            .expect("router responds");

        assert_problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_operation",
        )
        .await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 1);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);

        let issued = state
            .csrf_signer
            .issue(
                CsrfPrincipalBinding::from_verified(&principal),
                &access_token,
                now,
            )
            .expect("same session retains its CSRF token");
        let tampered_token = format!("{}x", issued.value);
        let tampered_cookie = format!("{}={tampered_token}", crate::csrf::csrf_cookie_name());
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/workspace/v2/workspaces/ws-1/products/invocations")
            .header(ORIGIN, "https://client.example")
            .header(
                http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header(COOKIE, tampered_cookie)
            .header(CSRF_HEADER, tampered_token)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(Body::from(
                r#"{"ownerId":"unlisted","operationId":"unknown"}"#,
            ))
            .expect("request is valid");
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");

        assert_problem(response, StatusCode::FORBIDDEN, "csrf_failed").await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 2);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn legacy_v1_product_route_is_not_mounted() {
        let (state, verifier_calls, gateway_calls) = test_state();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/workspace/v1/workspaces/ws-1/products/NOT_A_WORKSPACE_OPERATION")
            .header(ORIGIN, "https://client.example")
            .body(Body::empty())
            .expect("request is valid");
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn externally_composed_browser_routes_require_verified_session_context() {
        async fn credential_handler(State(calls): State<Arc<AtomicUsize>>) -> StatusCode {
            calls.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }

        let (state, verifier_calls, _) = test_state();
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let routes = Router::new()
            .route("/credential-registration", any(credential_handler))
            .with_state(Arc::clone(&handler_calls));
        let app = with_verified_web_session_routes(state, routes);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/credential-registration")
            .header(ORIGIN, "https://client.example")
            .body(Body::empty())
            .expect("request is valid");

        let response = app.oneshot(request).await.expect("router responds");
        assert_problem(response, StatusCode::UNAUTHORIZED, "unauthenticated").await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn v2_query_parameters_are_rejected_before_authentication() {
        let (state, verifier_calls, gateway_calls) = test_state();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/workspace/v2/workspaces/ws-1/products/invocations?ownerId=echo")
            .header(ORIGIN, "https://client.example")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"ownerId":"echo","operationId":"workspaceGetEvaluationSuite"}"#,
            ))
            .expect("request is valid");
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_request").await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }
}
