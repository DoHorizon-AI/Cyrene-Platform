//! Catalyst Product HTTP route mapping.
//!
//! Dataset resources and their lifecycle remain owned by Catalyst. These
//! routes only translate the closed Workspace operation keys to OpenAPI v1.
//! / Dataset 与生命周期状态仍归 Catalyst 所有；此处仅映射固定路由。

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};

use crate::{ProductInvocationError, ProductInvocationRequest};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

/// Maps only Catalyst `listDatasets` and `createDataset` to fixed Product paths.
pub(super) fn target(
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Catalyst {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation01, Kind::Read) => {
            if request.resource_id.is_some()
                || !request.json_body.is_empty()
                || request.idempotency_key.is_some()
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Catalyst,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("internal"),
                    ProductHttpPathSegment::Static("workspace"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("datasets"),
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation02, Kind::Command) => {
            if request.resource_id.is_some()
                || request.json_body.is_empty()
                || !is_json_object(&request.json_body)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Catalyst,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("internal"),
                    ProductHttpPathSegment::Static("workspace"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("datasets"),
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
            owner: Owner::Catalyst,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body: json_body.to_vec(),
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    #[test]
    fn list_datasets_maps_to_fixed_collection_get() {
        let route = target(&invocation(
            Operation::WorkspaceProductApiOperation01,
            Kind::Read,
            None,
            b"",
            None,
        ))
        .unwrap();

        assert_eq!(route.owner(), Owner::Catalyst);
        assert_eq!(route.method(), ProductHttpMethod::Get);
        assert_eq!(
            route
                .path_segments()
                .iter()
                .map(ProductHttpPathSegment::test_value)
                .collect::<Vec<_>>(),
            ["internal", "workspace", "v1", "datasets"]
        );
    }

    #[test]
    fn create_dataset_maps_to_fixed_collection_post_and_keeps_key() {
        let route = target(&invocation(
            Operation::WorkspaceProductApiOperation02,
            Kind::Command,
            None,
            br#"{"name":"dataset"}"#,
            Some("dataset-create-1"),
        ))
        .unwrap();

        assert_eq!(route.owner(), Owner::Catalyst);
        assert_eq!(route.method(), ProductHttpMethod::Post);
        assert_eq!(
            route
                .path_segments()
                .iter()
                .map(ProductHttpPathSegment::test_value)
                .collect::<Vec<_>>(),
            ["internal", "workspace", "v1", "datasets"]
        );
    }

    #[test]
    fn list_rejects_caller_supplied_route_data() {
        let request = invocation(
            Operation::WorkspaceProductApiOperation01,
            Kind::Read,
            Some("caller-path"),
            b"",
            None,
        );
        assert_eq!(
            target(&request).unwrap_err(),
            ProductInvocationError::InvalidRequest
        );
    }
}
