//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/host.rs             ║
//! ║ Module: cy_workspace_web_bff::host                                 ║
//! ║ Role: Expose liveness and fail-closed readiness probes.             ║
//! ║                                                                    ║
//! ║ 模块职责：提供存活探针与 fail-closed 就绪探针。                       ║
//! ╚══════════════════════════════════════════════════════════════════════╝

use axum::body::Body;
use axum::routing::get;
use axum::Router;
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{Response, StatusCode};

/// Build the probe-only router used until production dependencies are composed.
///
/// Liveness confirms that the process can accept HTTP. Readiness stays unavailable in this
/// slice; the production composition commit replaces this router only after its required
/// identity, Directory, contract, and Relay dependencies are constructed.
///
/// 构造 provider 尚未装配时使用的 probe-only router。此阶段仅确认进程存活，readiness 始终关闭。
pub(crate) fn health_router() -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(not_ready))
}

async fn health() -> Response<Body> {
    probe_response(StatusCode::OK, br#"{"status":"live"}"#)
}

async fn not_ready() -> Response<Body> {
    probe_response(
        StatusCode::SERVICE_UNAVAILABLE,
        br#"{"status":"not_ready"}"#,
    )
}

fn probe_response(status: StatusCode, body: &'static [u8]) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, http::HeaderValue::from_static("no-store"));
    response
}
