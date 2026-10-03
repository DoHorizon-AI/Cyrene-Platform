//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Trusted Product authorization policy                               │
//! │  Module: cy_workspace_product_contracts::policy                     │
//! │  Role: Enforce Platform-owned grants and mandatory scope bindings.   │
//! │                                                                     │
//! │  模块职责：执行 Platform 独立授权规则与必须保留的 scope selectors。   │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::bundle::{
    CatalogError, JsonScopeBinding, ProductBundlePins, ProductContractBundle, ProductOperation,
};
use crate::invocation::{
    invocation_fields, validate_scope_bindings, AuthorizedProductInvocation,
    AuthorizedProductInvocationInput, ProductInvocationError,
};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const MAX_SCOPE_ID_BYTES: usize = 512;

/// Errors kept distinct for authorization and safe transport mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationError {
    /// The supplied Product bundle is absent or has not been pinned successfully.
    #[error("PRODUCT_AUTH_CATALOG_INVALID")]
    Catalog(CatalogError),
    /// The trusted Platform policy is malformed or does not match its release pin.
    #[error("PRODUCT_AUTH_POLICY_INVALID")]
    Policy(CatalogError),
    /// No operation resolves from the loaded owner catalogs.
    #[error("PRODUCT_AUTH_UNKNOWN_OPERATION")]
    UnknownOperation,
    /// The operation exists but has no explicit Platform authorization grant.
    #[error("PRODUCT_AUTH_UNAPPROVED_OPERATION")]
    UnapprovedOperation,
    /// The authenticated principal class is not allowed by the grant.
    #[error("PRODUCT_AUTH_PRINCIPAL_NOT_ALLOWED")]
    PrincipalNotAllowed,
    /// The verified Directory roles do not satisfy the grant.
    #[error("PRODUCT_AUTH_ROLE_MISSING")]
    MissingRole,
    /// Organization, Workspace, or resource scope did not match the grant.
    #[error("PRODUCT_AUTH_SCOPE_MISMATCH")]
    ScopeMismatch,
    /// Request body, resource identifier, idempotency key, or selector is invalid.
    #[error("PRODUCT_AUTH_INVALID_REQUEST")]
    InvalidRequest,
}

/// Platform-verified principal class used by the policy evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProductPrincipalKind {
    /// Interactive user with current Directory membership and role lookup.
    DirectoryUser,
    /// A specifically authenticated trusted service workload.
    TrustedNavigatorWorkload,
}

/// Principal data created after authentication and authoritative Directory lookup.
#[derive(Debug, Clone)]
pub struct VerifiedProductPrincipal {
    kind: ProductPrincipalKind,
    roles: BTreeSet<String>,
}

impl VerifiedProductPrincipal {
    /// Creates an interactive principal from server-authenticated identity and
    /// roles returned by the authoritative Directory for the exact Workspace.
    pub fn directory_user(roles: impl IntoIterator<Item = String>) -> Self {
        Self {
            kind: ProductPrincipalKind::DirectoryUser,
            roles: roles.into_iter().collect(),
        }
    }

    /// Creates the principal only after validating the workload identity,
    /// audience, and exact Workspace scope server-side.
    pub fn trusted_navigator_workload() -> Self {
        Self {
            kind: ProductPrincipalKind::TrustedNavigatorWorkload,
            roles: BTreeSet::new(),
        }
    }

    /// Returns the verified principal class.
    pub fn kind(&self) -> ProductPrincipalKind {
        self.kind
    }

    /// Returns the server-derived Directory role set.
    pub fn roles(&self) -> &BTreeSet<String> {
        &self.roles
    }
}

/// Verified organization and Workspace scope from the authenticated server context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedWorkspaceScope {
    organization_id: String,
    workspace_id: String,
}

impl TrustedWorkspaceScope {
    /// Creates an exact scope after the caller has verified membership.
    pub fn new(
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Result<Self, AuthorizationError> {
        let organization_id = organization_id.into();
        let workspace_id = workspace_id.into();
        if !valid_scope_id(&organization_id) || !valid_scope_id(&workspace_id) {
            return Err(AuthorizationError::ScopeMismatch);
        }
        Ok(Self {
            organization_id,
            workspace_id,
        })
    }

    /// Returns the verified organization ID.
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the verified Workspace ID.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

/// Pinned Platform-owned authorization grants and mandatory scope selectors.
#[derive(Debug)]
pub struct TrustedProductPolicy {
    policy_version: String,
    grants: BTreeMap<(String, String), PolicyGrant>,
}

impl TrustedProductPolicy {
    /// Loads a separately pinned policy file from a server-owned path.
    pub fn load(path: impl AsRef<Path>, pins: &ProductBundlePins) -> Result<Self, CatalogError> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path).map_err(|_| CatalogError::FileInvalid)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() as usize > MAX_POLICY_BYTES
        {
            return Err(CatalogError::FileInvalid);
        }
        let bytes = fs::read(path).map_err(|_| CatalogError::FileInvalid)?;
        if !constant_time_hex_eq(&sha256_hex(&bytes), pins.policy_sha256()) {
            return Err(CatalogError::PinMismatch);
        }
        let document: PolicyDocument =
            serde_json::from_slice(&bytes).map_err(|_| CatalogError::CatalogInvalid)?;
        if document.schema_version != pins.policy_schema_version()
            || document.policy_version.is_empty()
        {
            return Err(CatalogError::CatalogInvalid);
        }
        let mut grants = BTreeMap::new();
        for grant in document.grants {
            if grant.owner_id.is_empty()
                || grant.operation_id.is_empty()
                || grant.owner_id.len() > 63
                || grant.operation_id.len() > 256
                || grant.principal_kinds.is_empty()
                || !grant.requires_exact_workspace
                || grant.required_roles.iter().any(String::is_empty)
                || grant.required_roles.iter().collect::<BTreeSet<_>>().len()
                    != grant.required_roles.len()
                || grant.principal_kinds.iter().collect::<BTreeSet<_>>().len()
                    != grant.principal_kinds.len()
                || (grant
                    .principal_kinds
                    .contains(&ProductPrincipalKind::TrustedNavigatorWorkload)
                    && !grant.required_roles.is_empty())
            {
                return Err(CatalogError::CatalogInvalid);
            }
            ensure_unique_bindings(&grant.required_request_bindings)?;
            ensure_unique_bindings(&grant.required_response_bindings)?;
            let key = (grant.owner_id.clone(), grant.operation_id.clone());
            if grants.insert(key, grant).is_some() {
                return Err(CatalogError::DuplicateOperation);
            }
        }
        Ok(Self {
            policy_version: document.policy_version,
            grants,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn empty_for_test() -> Self {
        Self {
            policy_version: "2.0.0".to_string(),
            grants: BTreeMap::new(),
        }
    }

    /// Returns the pinned Platform policy version.
    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    /// Returns whether an operation has a grant. This is a preflight check only.
    pub fn has_grant(&self, owner_id: &str, operation_id: &str) -> bool {
        self.grants
            .contains_key(&(owner_id.to_owned(), operation_id.to_owned()))
    }

    /// Checks every approved grant against the same pinned bundle used at runtime.
    /// New catalog operations remain denied until policy data explicitly grants them.
    pub fn validate_bundle(&self, bundle: &ProductContractBundle) -> Result<(), CatalogError> {
        for ((owner_id, operation_id), grant) in &self.grants {
            let operation = bundle
                .operation(owner_id, operation_id)
                .ok_or(CatalogError::CatalogInvalid)?;
            enforce_mandatory_bindings(grant, operation)?;
        }
        Ok(())
    }

    /// Authorizes a generic protobuf invocation and issues an opaque call token.
    pub fn authorize(
        &self,
        bundle: &ProductContractBundle,
        request: ProductApiInvocationV2,
        principal: &VerifiedProductPrincipal,
        scope: TrustedWorkspaceScope,
    ) -> Result<AuthorizedProductInvocation, AuthorizationError> {
        let (owner_id, operation_id, body_bytes, resource_id, idempotency_key) =
            invocation_fields(request);
        let operation = bundle
            .operation(&owner_id, &operation_id)
            .ok_or(AuthorizationError::UnknownOperation)?;
        let grant = self
            .grants
            .get(&(owner_id, operation_id))
            .ok_or(AuthorizationError::UnapprovedOperation)?;
        if !grant.principal_kinds.contains(&principal.kind) {
            return Err(AuthorizationError::PrincipalNotAllowed);
        }
        if !grant
            .required_roles
            .iter()
            .all(|role| principal.roles.contains(role))
        {
            return Err(AuthorizationError::MissingRole);
        }
        if !grant.requires_exact_workspace {
            return Err(AuthorizationError::ScopeMismatch);
        }
        enforce_mandatory_bindings(grant, operation).map_err(AuthorizationError::Catalog)?;

        operation
            .validate_resource_id(resource_id.as_deref())
            .map_err(|_| AuthorizationError::InvalidRequest)?;
        operation
            .validate_idempotency_key(idempotency_key.as_deref())
            .map_err(|_| AuthorizationError::InvalidRequest)?;
        let parsed_body = operation
            .validate_request(body_bytes.as_deref())
            .map_err(|_| AuthorizationError::InvalidRequest)?;
        validate_scope_bindings(
            &operation.scope_bindings().request_bindings,
            parsed_body.as_ref(),
            scope.organization_id(),
            scope.workspace_id(),
            resource_id.as_deref(),
        )
        .map_err(|error| match error {
            ProductInvocationError::PermissionDenied => AuthorizationError::ScopeMismatch,
            _ => AuthorizationError::InvalidRequest,
        })?;

        let mut path_parameters = BTreeMap::new();
        if let Some(name) = operation
            .scope_bindings()
            .organization_path_parameter
            .as_ref()
        {
            path_parameters.insert(name.clone(), scope.organization_id().to_owned());
        }
        if let Some(name) = operation.scope_bindings().workspace_path_parameter.as_ref() {
            path_parameters.insert(name.clone(), scope.workspace_id().to_owned());
        }
        if let Some(name) = operation.resource_id().path_parameter.as_ref() {
            if let Some(resource_id) = resource_id.as_ref() {
                path_parameters.insert(name.clone(), resource_id.clone());
            }
        }

        Ok(AuthorizedProductInvocation::issue(
            AuthorizedProductInvocationInput {
                operation: operation.clone(),
                organization_id: scope.organization_id().to_owned(),
                workspace_id: scope.workspace_id().to_owned(),
                resource_id,
                idempotency_key,
                json_body_bytes: body_bytes,
                json_body: parsed_body,
                path_parameters,
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyDocument {
    schema_version: String,
    policy_version: String,
    grants: Vec<PolicyGrant>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyGrant {
    owner_id: String,
    operation_id: String,
    principal_kinds: Vec<ProductPrincipalKind>,
    required_roles: Vec<String>,
    requires_exact_workspace: bool,
    required_request_bindings: Vec<JsonScopeBinding>,
    required_response_bindings: Vec<JsonScopeBinding>,
}

fn enforce_mandatory_bindings(
    grant: &PolicyGrant,
    operation: &ProductOperation,
) -> Result<(), CatalogError> {
    if !contains_all(
        &operation.scope_bindings().request_bindings,
        &grant.required_request_bindings,
    ) || !contains_all(
        &operation.scope_bindings().response_bindings,
        &grant.required_response_bindings,
    ) {
        return Err(CatalogError::CatalogInvalid);
    }
    Ok(())
}

fn contains_all(actual: &[JsonScopeBinding], required: &[JsonScopeBinding]) -> bool {
    required.iter().all(|binding| actual.contains(binding))
}

fn ensure_unique_bindings(bindings: &[JsonScopeBinding]) -> Result<(), CatalogError> {
    let unique = bindings.iter().collect::<BTreeSet<_>>();
    if unique.len() != bindings.len() {
        return Err(CatalogError::CatalogInvalid);
    }
    Ok(())
}

fn valid_scope_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_ID_BYTES
        && value != "."
        && value != ".."
        && !value.contains("..")
        && value.chars().all(|character| {
            !character.is_control() && !matches!(character, '/' | '\\' | '%' | '?' | '#')
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn constant_time_hex_eq(actual: &str, expected: &str) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}
