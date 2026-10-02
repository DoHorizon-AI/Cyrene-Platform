//! Private HTTP mount point for the WorkspaceDevice certificate-rotation route.
//!
//! This router is deliberately separate from the public device-enrollment
//! router and is not mounted by any host. A future host may mount it only when
//! its inbound mTLS layer injects `AuthenticatedRelayWorkspaceDevice` after
//! certificate-chain, revocation, and current Directory-binding validation.
//! Raw XFCC or other request-supplied forwarding headers are never evidence.

use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use axum::Router;

use super::{
    begin_device_certificate_rotation, DeviceCertificateRotationHttpDependencies,
    DeviceCertificateRotationHttpState, MAX_HTTP_BODY_BYTES,
};

/// Builds the private rotation route without mounting it into an application.
///
/// If the atomic Directory/authorization provider is absent, an authenticated
/// request receives the fixed generic 503 API error. Hosts must not expose this
/// router until the trusted mTLS marker source and generation/revocation checks
/// are proven end to end.
#[allow(dead_code)]
pub(crate) fn private_mtls_device_certificate_rotation_router(
    dependencies: DeviceCertificateRotationHttpDependencies,
) -> Router {
    Router::new()
        .route(
            "/v1/workspace-devices/:device_id/certificate-rotations",
            post(begin_device_certificate_rotation),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(DeviceCertificateRotationHttpState::new(dependencies))
}
