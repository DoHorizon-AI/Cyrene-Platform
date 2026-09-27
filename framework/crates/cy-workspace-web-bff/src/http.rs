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
use chrono::{DateTime, SecondsFormat, Utc};
use cy_workspace_fabric::workspace_v1::{
    WorkspaceConnectionDescriptor, WorkspaceProductApiOperation,
};
use cy_workspace_fabric::{
    validate_descriptor, VerifiedWebPrincipal, WebIdentityError, WebPrincipalVerifier,
    WorkspaceDirectory, WorkspaceDirectoryError,
};
use http::header::{
    ACCEPT, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use url::form_urlencoded;
use uuid::Uuid;

use crate::csrf::{csrf_set_cookie, CsrfPrincipalBinding, CsrfSigner};
use crate::device_approval::{device_approval_router, DeviceApprovalDependencies};
use crate::manifest::product_projection_manifest;
use crate::problem::{problem_response, ProblemCode};
use crate::product::{
    product_response, workspace_product_request, ProductOperationCatalog, ProductOperationContract,
    WorkspaceGatewayError, WorkspaceProductGateway,
};

/// Maximum UTF-8 JSON request or response body size, in bytes.
pub const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACCEPT_HEADER_BYTES: usize = 512;
const MAX_QUERY_BYTES: usize = 2048;
const UNTRUSTED_EASY_AUTH_HEADER_PREFIX: &str = "x-ms-token-";
const TRACEPARENT_HEADER: &str = "traceparent";
const CSRF_HEADER: &str = "x-csrf-token";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

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
    /// The canonical TCK does not match the compiled Workspace operation enum.
    #[error("Product operation manifest is inconsistent")]
    Manifest,
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
            .field("product_operation_count", &self.product_operations.len())
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
        product_projection_manifest().map_err(|_| WebBffStartupError::Manifest)?;
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
            "/api/workspace/v1/workspaces/:workspace_id/products/:operation",
            any(product_route),
        )
        .with_state(state.clone());
    let approval_routes = device_approval_router(device_approval).route_layer(
        middleware::from_fn_with_state(state.clone(), device_approval_session),
    );

    Router::new()
        .merge(core_routes)
        .merge(approval_routes)
        .fallback(not_found_route)
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

struct TraceInfo {
    traceparent: String,
    trace_id: String,
}

struct AuthenticatedRequest {
    principal: VerifiedWebPrincipal,
    access_token: String,
}

/// Authenticate one browser approval command and insert its typed session context.
///
/// This middleware runs only on the three approval routes. It verifies the exact
/// configured Origin, bearer identity, and matching signed CSRF header/cookie
/// before inserting the context; raw credential headers are then removed before
/// the route handler runs.
///
/// 仅对三个审批命令验证精确 Origin、Bearer identity 与签名 CSRF header/cookie，成功后注入 typed context，
/// 并在进入 handler 前移除原始 credential header。
async fn device_approval_session(
    State(state): State<Arc<WebBffState>>,
    mut request: Request<Body>,
    next: Next,
) -> http::Response<Body> {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
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
    Path((workspace_id, operation_name)): Path<(String, String)>,
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
    if !exact_origin_matches(request.headers(), &state.client_origin) {
        return forbidden(Some(&trace.trace_id));
    }
    if workspace_id.is_empty() || workspace_id.len() > 200 {
        return invalid_request(Some(&trace.trace_id));
    }
    let operation = match WorkspaceProductApiOperation::from_str_name(&operation_name) {
        Some(value) if value != WorkspaceProductApiOperation::Unspecified => value,
        _ => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::UnsupportedOperation,
                "Unprocessable Content",
                Some(&trace.trace_id),
            );
        }
    };
    if state.product_operations.is_deny_only(operation) {
        return forbidden(Some(&trace.trace_id));
    }
    let Some(contract) = state.product_operations.get(operation) else {
        return problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ProblemCode::UnsupportedOperation,
            "Unprocessable Content",
            Some(&trace.trace_id),
        );
    };
    let resource_id = match parse_resource_id(request.uri()) {
        Ok(value) => value,
        Err(()) => return invalid_request(Some(&trace.trace_id)),
    };
    let idempotency_key = match parse_idempotency_key(request.headers()) {
        Ok(value) => value,
        Err(()) => return invalid_request(Some(&trace.trace_id)),
    };
    if contract.requires_idempotency_key && idempotency_key.is_none() {
        return invalid_request(Some(&trace.trace_id));
    }
    if idempotency_key.is_some() && !contract.allows_idempotency_key {
        return invalid_request(Some(&trace.trace_id));
    }
    if let Some(value) = idempotency_key.as_ref() {
        let Some(schema) = contract.idempotency_key_schema.as_ref() else {
            return internal_error(Some(&trace.trace_id));
        };
        if !schema.validate(&Value::String(value.clone())) {
            return invalid_request(Some(&trace.trace_id));
        }
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
    if contract.projection.kind
        == cy_workspace_fabric::workspace_v1::WorkspaceProductApiRequestKind::Command
    {
        let header = match single_header(request.headers(), CSRF_HEADER) {
            Ok(value) => value,
            Err(()) => return csrf_failed(Some(&trace.trace_id)),
        };
        let cookie = match csrf_cookie_from_header(request.headers()) {
            Ok(value) => value,
            Err(()) => return csrf_failed(Some(&trace.trace_id)),
        };
        if header != cookie
            || state
                .csrf_signer
                .verify(
                    header,
                    CsrfPrincipalBinding::from_verified(&authenticated.principal),
                    &authenticated.access_token,
                    now,
                )
                .is_err()
        {
            return csrf_failed(Some(&trace.trace_id));
        }
    }
    let body = match read_request_json(request, contract).await {
        Ok(value) => value,
        Err(failure) => return failure.response(Some(&trace.trace_id)),
    };
    let path = match map_path_parameters(contract, &workspace_id, resource_id.as_deref()) {
        Ok(value) => value,
        Err(()) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::UnsupportedOperation,
                "Unprocessable Content",
                Some(&trace.trace_id),
            );
        }
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
    let forwarded_idempotency_key = if contract.allows_idempotency_key {
        idempotency_key.as_deref()
    } else {
        None
    };
    let request_id = Uuid::new_v4().to_string();
    let workspace_request = workspace_product_request(
        &contract.projection,
        &workspace_id,
        path.resource_id.as_deref(),
        body.as_deref(),
        forwarded_idempotency_key,
        &trace.traceparent,
        request_id.clone(),
    );
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
    let value: Value = match serde_json::from_slice(&response_body) {
        Ok(value) => value,
        Err(_) => return invalid_upstream_response(Some(&trace.trace_id)),
    };
    if validate_product_response(contract, status, content_type, &value).is_err() {
        return invalid_upstream_response(Some(&trace.trace_id));
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

struct ParsedPath {
    resource_id: Option<String>,
}

fn map_path_parameters(
    contract: &ProductOperationContract,
    workspace_id: &str,
    resource_id: Option<&str>,
) -> Result<ParsedPath, ()> {
    if contract.has_unsupported_query_or_header_parameters {
        return Err(());
    }
    let workspace_parameters = contract
        .path_parameters
        .iter()
        .filter(|parameter| matches!(parameter.name.as_str(), "workspace_id" | "workspaceId"))
        .collect::<Vec<_>>();
    if workspace_parameters.len() > 1 {
        return Err(());
    }
    let resource_parameters = contract
        .path_parameters
        .iter()
        .filter(|parameter| !matches!(parameter.name.as_str(), "workspace_id" | "workspaceId"))
        .collect::<Vec<_>>();
    if resource_parameters.len() > 1 {
        return Err(());
    }
    let path_schema = contract.path_schema.as_ref();
    if let Some(parameter) = workspace_parameters.first() {
        let schema = path_schema.ok_or(())?;
        if !schema.validate_path_parameter(&parameter.name, workspace_id) {
            return Err(());
        }
    }
    match resource_parameters.first() {
        Some(parameter) => {
            if parameter.required && resource_id.is_none() {
                return Err(());
            }
            if let Some(value) = resource_id {
                let schema = path_schema.ok_or(())?;
                if !schema.validate_path_parameter(&parameter.name, value) {
                    return Err(());
                }
            }
            Ok(ParsedPath {
                resource_id: resource_id.map(str::to_owned),
            })
        }
        None if resource_id.is_none() => Ok(ParsedPath { resource_id: None }),
        None => Err(()),
    }
}

fn validate_product_response(
    contract: &ProductOperationContract,
    status: u16,
    content_type: &str,
    value: &Value,
) -> Result<(), ()> {
    let schema = contract.response.schema.as_ref().ok_or(())?;
    if !schema.validate_response(status, value) {
        return Err(());
    }
    if status >= 400 {
        if content_type == "application/problem+json" {
            validate_public_problem_details(status, value)?;
        }
        if contains_unsafe_error_address(value) {
            return Err(());
        }
    }
    let fields = contract
        .response
        .resource_reference_fields
        .as_ref()
        .ok_or(())?;
    let manifest = product_projection_manifest().map_err(|_| ())?;
    validate_nested_resource_references(value, &manifest)?;
    for field in fields {
        if !field.json_pointer.starts_with('/') {
            return Err(());
        }
        match value.pointer(&field.json_pointer) {
            None if field.required => return Err(()),
            None => {}
            Some(reference) => validate_resource_reference(reference, &manifest)?,
        }
    }
    Ok(())
}

fn validate_resource_reference(
    value: &Value,
    manifest: &[crate::ProductProjectionEntry],
) -> Result<(), ()> {
    let object = value.as_object().ok_or(())?;
    if object.len() != 2 || !object.contains_key("operation") || !object.contains_key("resourceId")
    {
        return Err(());
    }
    let operation_name = object.get("operation").and_then(Value::as_str).ok_or(())?;
    let operation = WorkspaceProductApiOperation::from_str_name(operation_name).ok_or(())?;
    if !manifest.iter().any(|entry| {
        entry.operation == operation
            && entry.kind == cy_workspace_fabric::workspace_v1::WorkspaceProductApiRequestKind::Read
    }) {
        return Err(());
    }
    let resource_id = object.get("resourceId").and_then(Value::as_str).ok_or(())?;
    if !is_safe_opaque_resource_id(resource_id) {
        return Err(());
    }
    Ok(())
}

fn validate_nested_resource_references(
    value: &Value,
    manifest: &[crate::ProductProjectionEntry],
) -> Result<(), ()> {
    match value {
        Value::Object(object) => {
            if object.contains_key("operation") && object.contains_key("resourceId") {
                validate_resource_reference(value, manifest)?;
            }
            for child in object.values() {
                validate_nested_resource_references(child, manifest)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                validate_nested_resource_references(child, manifest)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_safe_opaque_resource_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value != "."
        && value != ".."
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
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

async fn read_request_json(
    request: Request<Body>,
    contract: &ProductOperationContract,
) -> Result<Option<Vec<u8>>, RequestFailure> {
    let content_length = content_length(request.headers())?;
    if content_length.is_some_and(|length| length > MAX_JSON_BODY_BYTES) {
        return Err(RequestFailure::PayloadTooLarge);
    }
    let json_content_type = is_json_content_type(request.headers());
    let bytes = match to_bytes(request.into_body(), MAX_JSON_BODY_BYTES).await {
        Ok(value) => value,
        Err(_) => return Err(RequestFailure::PayloadTooLarge),
    };
    if bytes.is_empty() {
        if contract.request_body.required {
            return Err(RequestFailure::InvalidShape);
        }
        return Ok(None);
    }
    if !json_content_type {
        return Err(RequestFailure::UnsupportedMediaType);
    }
    if !contract.request_body.allowed {
        return Err(RequestFailure::InvalidShape);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| RequestFailure::InvalidJson)?;
    let schema = contract
        .request_body
        .schema
        .as_ref()
        .ok_or(RequestFailure::InvalidShape)?;
    if !schema.validate(&value) {
        return Err(RequestFailure::InvalidShape);
    }
    Ok(Some(bytes.to_vec()))
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

enum RequestFailure {
    InvalidJson,
    UnsupportedMediaType,
    PayloadTooLarge,
    InvalidShape,
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
            Self::InvalidShape => problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::UnsupportedOperation,
                "Unprocessable Content",
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

fn parse_resource_id(uri: &Uri) -> Result<Option<String>, ()> {
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    if query.len() > MAX_QUERY_BYTES {
        return Err(());
    }
    let mut resource_id = None;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if key != "resourceId" || resource_id.is_some() || value.is_empty() || value.len() > 512 {
            return Err(());
        }
        resource_id = Some(value.into_owned());
    }
    Ok(resource_id)
}

fn parse_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ()> {
    match single_header(headers, IDEMPOTENCY_HEADER) {
        Ok(value) if value.is_empty() || value.len() > 200 => Err(()),
        Ok(value) => Ok(Some(value.to_owned())),
        Err(()) if !headers.contains_key(IDEMPOTENCY_HEADER) => Ok(None),
        Err(()) => Err(()),
    }
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
    use cy_workspace_fabric::workspace_v1::{UserIdentityRef, WorkspaceApiRequest};
    use cy_workspace_fabric::{WebIdentityError, WorkspaceDirectoryError};
    use http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::product::{
        ProductJsonSchema, ProductRequestBodyContract, ProductResourceReferenceField,
        ProductResponseContract,
    };

    struct AnyJsonSchema;

    impl ProductJsonSchema for AnyJsonSchema {
        fn validate(&self, _value: &Value) -> bool {
            true
        }
    }

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
        ) -> Result<cy_workspace_fabric::workspace_v1::WorkspaceApiResponse, WorkspaceGatewayError>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(WorkspaceGatewayError::Unavailable)
        }
    }

    fn test_catalog() -> ProductOperationCatalog {
        let manifest = product_projection_manifest().expect("manifest should parse");
        let contracts = manifest.into_iter().map(|projection| {
            let deny_only = projection.operation
                == WorkspaceProductApiOperation::WorkspaceProductApiOperation13;
            ProductOperationContract {
                projection,
                upstream_method: Method::POST,
                path_parameters: Vec::new(),
                path_schema: None,
                has_unsupported_query_or_header_parameters: false,
                request_body: ProductRequestBodyContract {
                    allowed: false,
                    required: false,
                    schema: None,
                },
                allows_idempotency_key: false,
                requires_idempotency_key: false,
                idempotency_key_schema: None,
                response: ProductResponseContract {
                    schema: (!deny_only)
                        .then(|| Arc::new(AnyJsonSchema) as Arc<dyn ProductJsonSchema>),
                    resource_reference_fields: (!deny_only)
                        .then(Vec::<ProductResourceReferenceField>::new),
                },
            }
        });
        ProductOperationCatalog::new(contracts).expect("complete catalog should validate")
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
    fn resource_query_is_single_and_bounded() {
        let uri: Uri = "/route?resourceId=opaque-1".parse().expect("valid uri");
        assert_eq!(
            parse_resource_id(&uri).expect("single id"),
            Some("opaque-1".to_owned())
        );
        let duplicate: Uri = "/route?resourceId=one&resourceId=two"
            .parse()
            .expect("valid uri");
        assert!(parse_resource_id(&duplicate).is_err());
        let unknown: Uri = "/route?owner=exchange".parse().expect("valid uri");
        assert!(parse_resource_id(&unknown).is_err());
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
    async fn every_product_post_requires_exact_origin_before_authentication() {
        let (state, verifier_calls, gateway_calls) = test_state();
        for origin in [None, Some("https://attacker.example")] {
            let mut request = Request::builder()
                .method(Method::POST)
                .uri(
                    "/api/workspace/v1/workspaces/ws-1/products/WORKSPACE_PRODUCT_API_OPERATION_01",
                )
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
    async fn unsupported_product_operation_is_rejected_without_provider_calls() {
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

        assert_problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_operation",
        )
        .await;
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gateway_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn product_request_body_is_capped_at_four_mib() {
        let catalog = test_catalog();
        let contract = catalog
            .get(
                WorkspaceProductApiOperation::from_str_name("WORKSPACE_PRODUCT_API_OPERATION_01")
                    .expect("canonical operation"),
            )
            .expect("catalog entry");
        let request = Request::builder()
            .method(Method::POST)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(vec![b'a'; MAX_JSON_BODY_BYTES + 1]))
            .expect("request is valid");

        let result = read_request_json(request, contract).await;
        assert!(matches!(result, Err(RequestFailure::PayloadTooLarge)));
    }
}
