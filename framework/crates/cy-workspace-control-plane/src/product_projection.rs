//! Bounded Product API v2 wire projection.
//!
//! This module keeps the protobuf boundary small. Operation identity, route,
//! schema, policy, and scope rules come from the shared contracts crate; no
//! Product operation is mapped in Platform code. / 此模块只处理受限的 v2 wire projection。

use cy_proto::cyrene::workspace::product::v2::{ProductApiInvocationV2, ProductApiResponseV2};
use cy_workspace_product_contracts::{
    parse_json_bytes_with_limit, ProductInvocationError, ProductInvocationResponse,
    PRODUCT_JSON_BYTES_LIMIT,
};

/// Maximum Product request or response JSON body size.
pub const PRODUCT_JSON_BODY_MAX_BYTES: usize = PRODUCT_JSON_BYTES_LIMIT;

/// Maximum encoded Workspace API gRPC message, including protobuf wrappers.
pub const WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

pub(crate) const MAX_WORKSPACE_REQUEST_ID_BYTES: usize = 128;
pub(crate) const MAX_WORKSPACE_ID_BYTES: usize = 512;
const MAX_OWNER_ID_BYTES: usize = 63;
const MAX_OPERATION_ID_BYTES: usize = 256;
const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;

/// Applies cheap wire bounds before catalog lookup, JSON parsing, or authorization.
///
/// Full schema, resource, idempotency, and scope validation is performed by the
/// source-pinned `TrustedProductPolicy` and `ProductContractBundle`.
pub(crate) fn validate_product_invocation(
    request: ProductApiInvocationV2,
) -> Result<ProductApiInvocationV2, ProductInvocationError> {
    if !valid_owner_id(&request.owner_id)
        || request.operation_id.is_empty()
        || request.operation_id.len() > MAX_OPERATION_ID_BYTES
        || !request.operation_id.is_ascii()
        || request.operation_id.chars().any(char::is_control)
        || request.json_body.len() > PRODUCT_JSON_BODY_MAX_BYTES
        || request.resource_id.len() > MAX_RESOURCE_ID_BYTES
        || request.resource_id.chars().any(char::is_control)
        || request.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(request)
}

/// Converts an already schema- and scope-validated response to the v2 wire type.
///
/// The control plane calls `AuthorizedProductInvocation::validate_response`
/// before this conversion. This final check keeps response bytes within the
/// Workspace RPC envelope and rejects invalid JSON media independently.
pub(crate) fn validate_product_response(
    response: ProductInvocationResponse,
) -> Result<ProductApiResponseV2, ProductInvocationError> {
    if (!(200..=299).contains(&response.status_code)
        && !(400..=599).contains(&response.status_code))
        || response.json_body.len() > PRODUCT_JSON_BODY_MAX_BYTES
    {
        return Err(ProductInvocationError::Internal);
    }
    if response.json_body.is_empty() {
        if !response.content_type.is_empty() && !valid_content_type(&response.content_type) {
            return Err(ProductInvocationError::Internal);
        }
    } else if !valid_content_type(&response.content_type)
        || parse_json_bytes_with_limit(&response.json_body, PRODUCT_JSON_BODY_MAX_BYTES).is_err()
    {
        return Err(ProductInvocationError::Internal);
    }

    Ok(ProductApiResponseV2 {
        status_code: u32::from(response.status_code),
        json_body: response.json_body,
        content_type: response.content_type,
    })
}

/// Maps a fixed adapter failure to the public Google RPC status projection.
pub(crate) const fn product_invocation_rpc_status(
    error: ProductInvocationError,
) -> (i32, &'static str) {
    match error {
        ProductInvocationError::InvalidRequest => (3, "WORKSPACE_PRODUCT_API_REQUEST_INVALID"),
        ProductInvocationError::PermissionDenied => (7, "WORKSPACE_PRODUCT_API_DENIED"),
        ProductInvocationError::NotFound => (5, "WORKSPACE_PRODUCT_API_NOT_FOUND"),
        ProductInvocationError::Conflict | ProductInvocationError::FailedPrecondition => {
            (9, "WORKSPACE_PRODUCT_API_FAILED_PRECONDITION")
        }
        ProductInvocationError::Unavailable => (14, "WORKSPACE_PRODUCT_API_UNAVAILABLE"),
        ProductInvocationError::Internal => (13, "WORKSPACE_PRODUCT_API_FAILURE"),
    }
}

fn valid_owner_id(value: &str) -> bool {
    value.len() <= MAX_OWNER_ID_BYTES
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' => index > 0,
            b'-' => index > 0 && index + 1 < value.len(),
            _ => false,
        })
}

fn valid_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or_default().trim();
    media_type.eq_ignore_ascii_case("application/json")
        || media_type.eq_ignore_ascii_case("application/problem+json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_bounds_reject_invalid_or_oversized_v2_fields() {
        let valid = ProductApiInvocationV2 {
            owner_id: "catalyst".to_owned(),
            operation_id: "workspaceListDatasets".to_owned(),
            ..Default::default()
        };
        assert!(validate_product_invocation(valid.clone()).is_ok());

        let mut invalid = valid.clone();
        invalid.owner_id = "Catalyst".to_owned();
        assert_eq!(
            validate_product_invocation(invalid),
            Err(ProductInvocationError::InvalidRequest)
        );

        let mut invalid = valid;
        invalid.idempotency_key = "bad\nkey".to_owned();
        assert_eq!(
            validate_product_invocation(invalid),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[test]
    fn response_projection_keeps_only_bounded_json_responses() {
        let response = ProductInvocationResponse {
            status_code: 201,
            content_type: "application/json".to_owned(),
            json_body: br#"{"id":"dataset-1"}"#.to_vec(),
        };
        let projected = validate_product_response(response).unwrap();
        assert_eq!(projected.status_code, 201);
        assert_eq!(projected.content_type, "application/json");
        assert_eq!(projected.json_body, br#"{"id":"dataset-1"}"#);

        let invalid = ProductInvocationResponse {
            status_code: 302,
            content_type: "application/json".to_owned(),
            json_body: b"{}".to_vec(),
        };
        assert_eq!(
            validate_product_response(invalid),
            Err(ProductInvocationError::Internal)
        );

        let ambiguous = ProductInvocationResponse {
            status_code: 200,
            content_type: "application/json".to_owned(),
            json_body: br#"{"scope":{"workspaceId":"workspace-1","workspaceId":"workspace-2"}}"#
                .to_vec(),
        };
        assert_eq!(
            validate_product_response(ambiguous),
            Err(ProductInvocationError::Internal)
        );
    }
}
