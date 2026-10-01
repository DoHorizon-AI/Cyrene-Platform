//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Authorized Product invocation values                              │
//! │  Module: cy_workspace_product_contracts::invocation                 │
//! │  Role: Preserve validated wire data and bind pinned routes.          │
//! │                                                                     │
//! │  模块职责：保留已校验 wire 数据，并绑定固定目录中的 Product 路由。    │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;

use async_trait::async_trait;
use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use serde_json::Value;

use crate::bundle::{JsonScopeBinding, MatchContextField, ProductOperation};

/// Stable errors that can cross the Workspace Product API boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProductInvocationError {
    /// The generic request did not satisfy its pinned operation contract.
    #[error("PRODUCT_INVOCATION_INVALID_REQUEST")]
    InvalidRequest,
    /// The authenticated caller is not authorized for this operation or scope.
    #[error("PRODUCT_INVOCATION_PERMISSION_DENIED")]
    PermissionDenied,
    /// The authorized owner resource does not exist.
    #[error("PRODUCT_INVOCATION_NOT_FOUND")]
    NotFound,
    /// The owner rejected an idempotency key or conflicting state transition.
    #[error("PRODUCT_INVOCATION_CONFLICT")]
    Conflict,
    /// The operation cannot run because a required precondition is not met.
    #[error("PRODUCT_INVOCATION_FAILED_PRECONDITION")]
    FailedPrecondition,
    /// The Product endpoint or its trusted configuration is unavailable.
    #[error("PRODUCT_INVOCATION_UNAVAILABLE")]
    Unavailable,
    /// The Product returned data that violated its pinned contract.
    #[error("PRODUCT_INVOCATION_INTERNAL")]
    Internal,
}

/// Resolved method, path template, and Platform-bound path parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProductRoute {
    /// HTTP method resolved from the source-pinned OpenAPI operation.
    pub method: String,
    /// URI path template resolved from the source-pinned OpenAPI operation.
    pub path_template: String,
    /// Server-derived values for every placeholder in `path_template`.
    pub path_parameters: BTreeMap<String, String>,
}

impl ResolvedProductRoute {
    pub(crate) fn new(method: String, path_template: String) -> Self {
        Self {
            method,
            path_template,
            path_parameters: BTreeMap::new(),
        }
    }

    /// Returns the pinned HTTP method.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Returns the pinned URI path template.
    pub fn path_template(&self) -> &str {
        &self.path_template
    }

    /// Returns all Platform-injected URI template parameters.
    pub fn path_parameters(&self) -> &BTreeMap<String, String> {
        &self.path_parameters
    }
}

/// Validated response returned by a Product transport adapter.
#[derive(Clone, PartialEq, Eq)]
pub struct ProductInvocationResponse {
    /// HTTP status code returned by the Product.
    pub status_code: u16,
    /// Product response media type, including any optional parameters.
    pub content_type: String,
    /// Exact JSON response bytes returned by the Product.
    pub json_body: Vec<u8>,
}

impl std::fmt::Debug for ProductInvocationResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductInvocationResponse")
            .field("status_code", &self.status_code)
            .field("content_type", &self.content_type)
            .field("body_bytes", &self.json_body.len())
            .finish()
    }
}

/// Transport implemented by an adapter using only an authorized opaque call.
#[async_trait]
pub trait ProductInvocationAdapter: Send + Sync {
    /// Dispatch one call through its pinned route and server-owned endpoint.
    async fn invoke(
        &self,
        request: &AuthorizedProductInvocation,
    ) -> Result<ProductInvocationResponse, ProductInvocationError>;
}

/// Opaque Platform-issued request. Its constructor and fields stay private so
/// callers cannot forge principal, scope, route, or catalog state.
pub struct AuthorizedProductInvocation {
    operation: ProductOperation,
    organization_id: String,
    workspace_id: String,
    resource_id: Option<String>,
    idempotency_key: Option<String>,
    json_body_bytes: Option<Vec<u8>>,
    json_body: Option<Value>,
    route: ResolvedProductRoute,
}

impl std::fmt::Debug for AuthorizedProductInvocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedProductInvocation")
            .field("owner_id", &self.operation.owner_id())
            .field("operation_id", &self.operation.operation_id())
            .field("organization_id", &"<bound>")
            .field("workspace_id", &"<bound>")
            .field("has_resource_id", &self.resource_id.is_some())
            .field("has_idempotency_key", &self.idempotency_key.is_some())
            .field("has_body", &self.json_body_bytes.is_some())
            .field(
                "body_bytes",
                &self.json_body_bytes.as_ref().map_or(0, Vec::len),
            )
            .finish()
    }
}

impl AuthorizedProductInvocation {
    /// Assembles validated request data before exposing an opaque call token.
    pub(crate) fn issue(input: AuthorizedProductInvocationInput) -> Self {
        let mut route = input.operation.route().clone();
        route.path_parameters = input.path_parameters;
        Self {
            operation: input.operation,
            organization_id: input.organization_id,
            workspace_id: input.workspace_id,
            resource_id: input.resource_id,
            idempotency_key: input.idempotency_key,
            json_body_bytes: input.json_body_bytes,
            json_body: input.json_body,
            route,
        }
    }

    /// Returns the owner identifier from the pinned catalog.
    pub fn owner_id(&self) -> &str {
        self.operation.owner_id()
    }

    /// Returns the operation identifier from the pinned catalog.
    pub fn operation_id(&self) -> &str {
        self.operation.operation_id()
    }

    /// Returns the exact organization bound by the trusted caller context.
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    /// Returns the exact Workspace bound by the trusted caller context.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the owner resource identifier, if this operation requires one.
    pub fn resource_id(&self) -> Option<&str> {
        self.resource_id.as_deref()
    }

    /// Returns the validated standard idempotency key, if present.
    pub fn idempotency_key(&self) -> Option<&str> {
        self.idempotency_key.as_deref()
    }

    /// Returns whether the caller supplied a JSON request body.
    pub fn has_body(&self) -> bool {
        self.json_body_bytes.is_some()
    }

    /// Returns the parsed body after schema and scope validation.
    pub fn json_body(&self) -> Option<&Value> {
        self.json_body.as_ref()
    }

    /// Returns the caller's original JSON bytes without reserialization.
    pub fn json_body_bytes(&self) -> Option<&[u8]> {
        self.json_body_bytes.as_deref()
    }

    /// Returns the pinned route and server-derived path parameters.
    pub fn route(&self) -> &ResolvedProductRoute {
        &self.route
    }

    /// Returns response selectors required by both catalog and Platform policy.
    pub fn response_scope_bindings(&self) -> &[JsonScopeBinding] {
        &self.operation.scope_bindings().response_bindings
    }

    /// Validates response schema and all mandatory response scope selectors.
    pub fn validate_response(
        &self,
        response: &ProductInvocationResponse,
    ) -> Result<(), ProductInvocationError> {
        let value = self.operation.validate_response(
            response.status_code,
            &response.content_type,
            &response.json_body,
        )?;
        validate_scope_bindings(
            self.response_scope_bindings(),
            value.as_ref(),
            &self.organization_id,
            &self.workspace_id,
            self.resource_id.as_deref(),
        )
        .map_err(|_| ProductInvocationError::Internal)
    }
}

/// Owned, already-authorized values used only by the Platform policy issuer.
pub(crate) struct AuthorizedProductInvocationInput {
    pub(crate) operation: ProductOperation,
    pub(crate) organization_id: String,
    pub(crate) workspace_id: String,
    pub(crate) resource_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) json_body_bytes: Option<Vec<u8>>,
    pub(crate) json_body: Option<Value>,
    pub(crate) path_parameters: BTreeMap<String, String>,
}

/// Validates required RFC 6901 selectors using a bounded `*` wildcard.
pub(crate) fn validate_scope_bindings(
    bindings: &[JsonScopeBinding],
    value: Option<&Value>,
    organization_id: &str,
    workspace_id: &str,
    resource_id: Option<&str>,
) -> Result<(), ProductInvocationError> {
    if bindings.is_empty() {
        return Ok(());
    }
    let value = value.ok_or(ProductInvocationError::InvalidRequest)?;
    for binding in bindings {
        let expected = match binding.matches {
            MatchContextField::OrganizationId => organization_id,
            MatchContextField::WorkspaceId => workspace_id,
            MatchContextField::ResourceId => {
                resource_id.ok_or(ProductInvocationError::InvalidRequest)?
            }
        };
        let selected = select_json_pointer(value, &binding.json_pointer)?;
        if selected.is_empty()
            || selected
                .iter()
                .any(|candidate| candidate.as_str() != Some(expected))
        {
            return Err(ProductInvocationError::InvalidRequest);
        }
    }
    Ok(())
}

fn select_json_pointer<'a>(
    root: &'a Value,
    pointer: &str,
) -> Result<Vec<&'a Value>, ProductInvocationError> {
    if pointer.len() > 1024 || !pointer.starts_with('/') {
        return Err(ProductInvocationError::InvalidRequest);
    }
    let segments = pointer
        .split('/')
        .skip(1)
        .map(unescape_pointer_segment)
        .collect::<Result<Vec<_>, _>>()?;
    if segments.len() > 64 {
        return Err(ProductInvocationError::InvalidRequest);
    }
    let mut current = vec![root];
    for segment in segments {
        let mut next = Vec::new();
        for value in current {
            match value {
                Value::Array(items) if segment == "*" => next.extend(items.iter()),
                Value::Object(items) if segment == "*" => next.extend(items.values()),
                Value::Array(items) => {
                    let index = segment
                        .parse::<usize>()
                        .map_err(|_| ProductInvocationError::InvalidRequest)?;
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
                return Err(ProductInvocationError::InvalidRequest);
            }
        }
        current = next;
    }
    if current.is_empty() {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(current)
}

fn unescape_pointer_segment(value: &str) -> Result<String, ProductInvocationError> {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '~' {
            match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                _ => return Err(ProductInvocationError::InvalidRequest),
            }
        } else {
            result.push(character);
        }
    }
    Ok(result)
}

/// Converts the public protobuf request into owned generic input fields.
pub(crate) fn invocation_fields(
    request: ProductApiInvocationV2,
) -> (
    String,
    String,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
) {
    let body = (!request.json_body.is_empty()).then_some(request.json_body);
    let resource_id = (!request.resource_id.is_empty()).then_some(request.resource_id);
    let idempotency_key = (!request.idempotency_key.is_empty()).then_some(request.idempotency_key);
    (
        request.owner_id,
        request.operation_id,
        body,
        resource_id,
        idempotency_key,
    )
}

#[cfg(test)]
mod tests {
    use super::{select_json_pointer, unescape_pointer_segment};
    use serde_json::json;

    #[test]
    fn wildcard_selects_every_response_scope_value() {
        let value = json!({"items": [{"workspaceId": "w-1"}, {"workspaceId": "w-1"}]});
        let selected = select_json_pointer(&value, "/items/*/workspaceId").unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|item| item.as_str() == Some("w-1")));
        assert!(select_json_pointer(&value, "/missing").is_err());
    }

    #[test]
    fn pointer_escapes_follow_rfc_6901() {
        assert_eq!(unescape_pointer_segment("a~1b~0c").unwrap(), "a/b~c");
        assert!(unescape_pointer_segment("~2").is_err());
    }
}
