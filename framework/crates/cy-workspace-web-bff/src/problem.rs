// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/problem.rs          ║
// ║ Module: cy_workspace_web_bff::problem                              ║
// ║ Role: Produce fixed, bounded RFC 9457 JSON problem responses.       ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::problem                                ║
// ║ 职责：生成固定且有界的 RFC 9457 JSON problem response。               ║
// ╚══════════════════════════════════════════════════════════════════════╝

use axum::body::Body;
use axum::response::Response;
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use http::{HeaderValue, StatusCode};
use serde::Serialize;

use crate::http::MAX_JSON_BODY_BYTES;

/// Stable BFF-owned error codes from the Web BFF OpenAPI contract.
///
/// Web BFF OpenAPI 合同定义的稳定 BFF 错误 code。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemCode {
    /// Missing or invalid authenticated access token.
    Unauthenticated,
    /// Invalid verified-principal shape or verification configuration.
    InvalidPrincipal,
    /// Missing scope, CSRF failure, origin rejection, or authorization denial.
    Forbidden,
    /// Current membership does not include the selected Workspace.
    WorkspaceNotFound,
    /// Malformed route, query, header, or JSON syntax.
    InvalidRequest,
    /// Request media type is not application/json.
    UnsupportedMediaType,
    /// Request JSON is larger than the contract limit.
    PayloadTooLarge,
    /// Operation key or owner request shape is not admitted.
    UnsupportedOperation,
    /// Generic internal failure.
    InternalError,
    /// CSRF validation failed.
    CsrfFailed,
    /// A configured rate limit rejected the request.
    RateLimited,
    /// Workspace or Product response was invalid or unsafe.
    InvalidUpstreamResponse,
    /// Trusted identity, Directory, or Workspace API is unavailable.
    UpstreamUnavailable,
    /// Workspace API request exceeded its deadline.
    UpstreamTimeout,
    /// A durable device-approval ceremony is no longer in the expected state.
    StateConflict,
}

impl ProblemCode {
    /// Stable lowercase wire code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidPrincipal => "invalid_principal",
            Self::Forbidden => "forbidden",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::InternalError => "internal_error",
            Self::CsrfFailed => "csrf_failed",
            Self::RateLimited => "rate_limited",
            Self::InvalidUpstreamResponse => "invalid_upstream_response",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::StateConflict => "state_conflict",
        }
    }

    /// Contract status associated with this stable problem code.
    pub const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::InvalidPrincipal | Self::InvalidRequest | Self::UnsupportedOperation => {
                StatusCode::BAD_REQUEST
            }
            Self::Forbidden | Self::CsrfFailed => StatusCode::FORBIDDEN,
            Self::WorkspaceNotFound => StatusCode::NOT_FOUND,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::StateConflict => StatusCode::CONFLICT,
            Self::InvalidUpstreamResponse => StatusCode::BAD_GATEWAY,
            Self::UpstreamUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProblemBody<'a> {
    #[serde(rename = "type")]
    problem_type: &'static str,
    title: &'static str,
    status: u16,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<&'a str>,
}

/// Construct one contract-shaped problem response with a closed and bounded body.
///
/// 构造符合合同、字段封闭且 body 有界的 problem response。
pub(crate) fn problem_response(
    status: StatusCode,
    code: ProblemCode,
    title: &'static str,
    trace_id: Option<&str>,
) -> Response {
    let body = ProblemBody {
        problem_type: "about:blank",
        title,
        status: status.as_u16(),
        code: code.as_str(),
        trace_id,
    };
    let (status, serialized) = match serde_json::to_vec(&body) {
        Ok(serialized) if serialized.len() <= MAX_JSON_BODY_BYTES => (status, serialized),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"type":"about:blank","title":"Internal Server Error","status":500,"code":"internal_error"}"#.to_vec(),
        ),
    };
    let mut response = Response::new(Body::from(serialized.clone()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&serialized.len().to_string()) {
        headers.insert(CONTENT_LENGTH, value);
    }
    response
}
