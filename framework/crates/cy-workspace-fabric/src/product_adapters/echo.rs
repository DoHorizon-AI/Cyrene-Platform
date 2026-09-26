//! Echo Product HTTP route mapping.
//!
//! EvaluationSuite identity and policy remain owned by Echo. This module maps
//! only the closed suite read/create operation keys from Product OpenAPI v1.
//! / EvaluationSuite 身份与策略仍由 Echo 拥有；此处仅映射固定契约路由。

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};
use uuid::Uuid;

use crate::{ProductInvocationError, ProductInvocationRequest};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

/// Maps only Echo `getEvaluationSuite` and `createEvaluationSuite` operations.
pub(super) fn target(
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Echo {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation09, Kind::Read) => {
            if !request.json_body.is_empty() || request.idempotency_key.is_some() {
                return Err(ProductInvocationError::InvalidRequest);
            }
            let suite_id = request
                .resource_id
                .as_deref()
                .and_then(|resource_id| Uuid::parse_str(resource_id).ok())
                .ok_or(ProductInvocationError::InvalidRequest)?;
            ProductHttpTarget::new(
                Owner::Echo,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("evaluation-suites"),
                    ProductHttpPathSegment::resource(&suite_id.to_string())?,
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation10, Kind::Command) => {
            if request.resource_id.is_some()
                || request.json_body.is_empty()
                || !is_json_object(&request.json_body)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Echo,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("evaluation-suites"),
                ],
            )
        }
        _ => Err(ProductInvocationError::InvalidRequest),
    }
}

fn is_json_object(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|value| value.is_object())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(
        operation: Operation,
        kind: Kind,
        resource_id: Option<&str>,
        json_body: &[u8],
        idempotency_key: Option<&str>,
    ) -> ProductInvocationRequest {
        ProductInvocationRequest {
            owner: Owner::Echo,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body: json_body.to_vec(),
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    #[test]
    fn get_evaluation_suite_uses_one_validated_uuid_segment() {
        let route = target(&invocation(
            Operation::WorkspaceProductApiOperation09,
            Kind::Read,
            Some("550e8400-e29b-41d4-a716-446655440000"),
            b"",
            None,
        ))
        .unwrap();

        assert_eq!(route.owner(), Owner::Echo);
        assert_eq!(route.method(), ProductHttpMethod::Get);
        assert_eq!(
            route
                .path_segments()
                .iter()
                .map(ProductHttpPathSegment::test_value)
                .collect::<Vec<_>>(),
            [
                "api",
                "v1",
                "evaluation-suites",
                "550e8400-e29b-41d4-a716-446655440000"
            ]
        );
    }

    #[test]
    fn create_evaluation_suite_maps_to_fixed_collection_post() {
        let route = target(&invocation(
            Operation::WorkspaceProductApiOperation10,
            Kind::Command,
            None,
            br#"{"name":"suite","evaluator":"exact_match.v1","expectedField":"expected","actualField":"actual","threshold":1.0}"#,
            Some("suite-create-1"),
        ))
        .unwrap();

        assert_eq!(route.owner(), Owner::Echo);
        assert_eq!(route.method(), ProductHttpMethod::Post);
        assert_eq!(
            route
                .path_segments()
                .iter()
                .map(ProductHttpPathSegment::test_value)
                .collect::<Vec<_>>(),
            ["api", "v1", "evaluation-suites"]
        );
    }

    #[test]
    fn suite_identifier_must_match_the_openapi_uuid_format() {
        let request = invocation(
            Operation::WorkspaceProductApiOperation09,
            Kind::Read,
            Some("../other-resource"),
            b"",
            None,
        );
        assert_eq!(
            target(&request).unwrap_err(),
            ProductInvocationError::InvalidRequest
        );
    }
}
