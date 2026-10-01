//! Platform authorization bridge for the pinned Product v2 policy.
//!
//! Identity, organization membership, current roles, and the Connector's fixed
//! scope are checked here before the shared policy can mint an opaque
//! invocation. Product operation names and grants are never hard-coded here.

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_product_contracts::{
    AuthorizationError, AuthorizedProductInvocation, ProductContractBundle, ProductInvocationError,
    TrustedProductPolicy, TrustedWorkspaceScope, VerifiedProductPrincipal,
};

use crate::{WorkspaceCallerContext, WorkspaceCallerPrincipal};

/// Resolves and authorizes a v2 invocation using current server-verified caller state.
///
/// Caller organization and Workspace must match the immutable Connector
/// binding. The existing membership fence runs before Directory-derived roles
/// are passed into the separate, digest-pinned Product policy.
pub(crate) fn authorize_product_invocation(
    bundle: &ProductContractBundle,
    policy: &TrustedProductPolicy,
    invocation: ProductApiInvocationV2,
    caller: &WorkspaceCallerContext,
    organization_id: &str,
    workspace_id: &str,
) -> Result<AuthorizedProductInvocation, ProductInvocationError> {
    if caller.organization_id() != organization_id
        || caller.workspace_id() != workspace_id
        || !matches!(caller.principal(), WorkspaceCallerPrincipal::User(_))
    {
        return Err(ProductInvocationError::PermissionDenied);
    }
    caller
        .authorize_workspace_read(workspace_id)
        .map_err(|_| ProductInvocationError::PermissionDenied)?;

    let principal = VerifiedProductPrincipal::directory_user(caller.roles().iter().cloned());
    let scope = TrustedWorkspaceScope::new(organization_id, workspace_id)
        .map_err(map_authorization_error)?;
    policy
        .authorize(bundle, invocation, &principal, scope)
        .map_err(map_authorization_error)
}

fn map_authorization_error(error: AuthorizationError) -> ProductInvocationError {
    match error {
        AuthorizationError::InvalidRequest => ProductInvocationError::InvalidRequest,
        AuthorizationError::UnknownOperation
        | AuthorizationError::UnapprovedOperation
        | AuthorizationError::PrincipalNotAllowed
        | AuthorizationError::MissingRole
        | AuthorizationError::ScopeMismatch => ProductInvocationError::PermissionDenied,
        AuthorizationError::Catalog(_) | AuthorizationError::Policy(_) => {
            ProductInvocationError::Unavailable
        }
    }
}
