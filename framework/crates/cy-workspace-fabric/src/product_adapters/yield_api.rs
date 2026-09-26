//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 yield_api.rs                                                    │
//! │  Module: cy_workspace_fabric::product_adapters::yield_api           │
//! │  Role: Fixed Product HTTP mappings for Yield draft and run actions.  │
//! │                                                                     │
//! │  模块职责：Yield 草稿读取与运行启动的固定 Product HTTP 映射。             │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};
use uuid::Uuid;

use crate::product_projection::{ProductInvocationError, ProductInvocationRequest};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

/// Maps only Yield `getDraft` and `startRun` operations to their fixed routes.
///
/// The shared client preserves Product response status and body, so Yield's
/// asynchronous `202 Accepted` response remains Product-owned. `startRun`
/// does not claim replay protection that the Yield API does not define.
///
/// # Errors
/// Returns a fixed invocation error for an unsupported operation, invalid
/// Product resource identity, or request fields absent from the OpenAPI route.
pub(super) fn target(
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Yield {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation03, Kind::Read) => {
            if !request.json_body.is_empty() || request.idempotency_key.is_some() {
                return Err(ProductInvocationError::InvalidRequest);
            }

            let resource_id = canonical_uuid(request.resource_id.as_deref())?;
            ProductHttpTarget::new(
                Owner::Yield,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("training-drafts"),
                    ProductHttpPathSegment::resource(&resource_id)?,
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation04, Kind::Command) => {
            if !request.json_body.is_empty() || request.idempotency_key.is_some() {
                // Yield's current OpenAPI declares neither a body nor an
                // Idempotency-Key header for `startRun`.
                return Err(ProductInvocationError::InvalidRequest);
            }

            let resource_id = canonical_uuid(request.resource_id.as_deref())?;
            ProductHttpTarget::new(
                Owner::Yield,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("training-drafts"),
                    ProductHttpPathSegment::resource(&resource_id)?,
                    ProductHttpPathSegment::Static("actions"),
                    ProductHttpPathSegment::Static("start"),
                ],
            )
        }
        _ => Err(ProductInvocationError::InvalidRequest),
    }
}

fn canonical_uuid(resource_id: Option<&str>) -> Result<String, ProductInvocationError> {
    let resource_id = resource_id.ok_or(ProductInvocationError::InvalidRequest)?;
    let parsed =
        Uuid::parse_str(resource_id).map_err(|_| ProductInvocationError::InvalidRequest)?;
    let canonical = parsed.hyphenated().to_string();
    if !canonical.eq_ignore_ascii_case(resource_id) {
        return Err(ProductInvocationError::InvalidRequest);
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use cy_proto::workspace_v1::{
        WorkspaceProductApiContentType as ContentType, WorkspaceProductApiOperation as Operation,
        WorkspaceProductApiOwner as Owner, WorkspaceProductApiRequestKind as Kind,
    };

    use super::*;
    use crate::product_adapters::http::{
        ConfiguredProductEndpointResolver, ProductEndpointConfig, ProductHttpClient,
        ProductHttpRequest, ProductHttpResponse, ProductHttpTransport,
    };

    struct ObservedRequest {
        owner: Owner,
        method: ProductHttpMethod,
        url: String,
        body: Vec<u8>,
        idempotency_key: Option<String>,
        debug: String,
    }

    struct RecordingTransport {
        request: Mutex<Option<ObservedRequest>>,
        response: Mutex<Option<ProductHttpResponse>>,
    }

    impl RecordingTransport {
        fn new(response: ProductHttpResponse) -> Self {
            Self {
                request: Mutex::new(None),
                response: Mutex::new(Some(response)),
            }
        }
    }

    #[tonic::async_trait]
    impl ProductHttpTransport for RecordingTransport {
        async fn send(
            &self,
            request: ProductHttpRequest,
        ) -> Result<ProductHttpResponse, ProductInvocationError> {
            *self.request.lock().expect("request mutex") = Some(ObservedRequest {
                owner: request.owner(),
                method: request.method(),
                url: request.url().as_str().to_owned(),
                body: request.body().to_vec(),
                idempotency_key: request.idempotency_key().map(str::to_owned),
                debug: format!("{request:?}"),
            });
            Ok(self
                .response
                .lock()
                .expect("response mutex")
                .take()
                .expect("transport should be called once"))
        }
    }

    fn test_client(
        response_status: u16,
        response_body: &str,
    ) -> (ProductHttpClient, Arc<RecordingTransport>) {
        let owner = Owner::Yield;
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            owner,
            "organization-1",
            "workspace-1",
            "https://yield.test/",
            "private-yield-bearer",
        )])
        .expect("fixed test endpoint should be valid");
        let transport = Arc::new(RecordingTransport::new(ProductHttpResponse::new(
            response_status,
            ContentType::ApplicationJson,
            response_body.as_bytes().to_vec(),
        )));
        let client = ProductHttpClient::with_transport(Arc::new(resolver), transport.clone());
        (client, transport)
    }

    fn invocation(
        operation: Operation,
        kind: Kind,
        resource_id: Option<&str>,
        json_body: &[u8],
        idempotency_key: Option<&str>,
    ) -> ProductInvocationRequest {
        ProductInvocationRequest {
            owner: Owner::Yield,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body: json_body.to_vec(),
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn get_draft_uses_its_fixed_route_and_preserves_the_product_record() {
        let body = r#"{"id":"11111111-1111-4111-8111-111111111111","state":"DRAFT"}"#;
        let (client, transport) = test_client(200, body);
        let request = invocation(
            Operation::WorkspaceProductApiOperation03,
            Kind::Read,
            Some("11111111-1111-4111-8111-111111111111"),
            &[],
            None,
        );

        let response = client
            .send(
                &crate::product_adapters::test_member_caller("organization-1", "workspace-1"),
                target(&request).expect("mapped target"),
                &request,
            )
            .await
            .expect("mock Product response");

        assert_eq!(response.status_code, 200);
        assert_eq!(response.json_body, body.as_bytes());
        let observed = transport
            .request
            .lock()
            .expect("request mutex")
            .take()
            .expect("request was dispatched");
        assert_eq!(observed.owner, Owner::Yield);
        assert_eq!(observed.method, ProductHttpMethod::Get);
        assert_eq!(
            observed.url,
            "https://yield.test/api/v1/training-drafts/11111111-1111-4111-8111-111111111111"
        );
        assert!(observed.body.is_empty());
        assert_eq!(observed.idempotency_key, None);
        assert!(!observed.debug.contains("private-yield-bearer"));
    }

    #[tokio::test]
    async fn start_run_uses_post_without_idempotency_and_keeps_accepted_status() {
        let body = r#"{"id":"22222222-2222-4222-8222-222222222222","state":"QUEUED"}"#;
        let (client, transport) = test_client(202, body);
        let request = invocation(
            Operation::WorkspaceProductApiOperation04,
            Kind::Command,
            Some("11111111-1111-4111-8111-111111111111"),
            &[],
            None,
        );

        let response = client
            .send(
                &crate::product_adapters::test_member_caller("organization-1", "workspace-1"),
                target(&request).expect("mapped target"),
                &request,
            )
            .await
            .expect("mock Product response");

        assert_eq!(response.status_code, 202);
        assert_eq!(response.json_body, body.as_bytes());
        let observed = transport
            .request
            .lock()
            .expect("request mutex")
            .take()
            .expect("request was dispatched");
        assert_eq!(observed.owner, Owner::Yield);
        assert_eq!(observed.method, ProductHttpMethod::Post);
        assert_eq!(
            observed.url,
            "https://yield.test/api/v1/training-drafts/11111111-1111-4111-8111-111111111111/actions/start"
        );
        assert!(observed.body.is_empty());
        assert_eq!(observed.idempotency_key, None);
        assert!(!observed.debug.contains("private-yield-bearer"));
    }

    #[test]
    fn rejects_non_openapi_yield_request_fields_and_path_ids() {
        let invalid_id = invocation(
            Operation::WorkspaceProductApiOperation03,
            Kind::Read,
            Some("11111111-1111-4111-8111-111111111111?other=1"),
            &[],
            None,
        );
        assert_eq!(
            target(&invalid_id),
            Err(ProductInvocationError::InvalidRequest)
        );

        let start_with_key = invocation(
            Operation::WorkspaceProductApiOperation04,
            Kind::Command,
            Some("11111111-1111-4111-8111-111111111111"),
            &[],
            Some("client-key"),
        );
        assert_eq!(
            target(&start_with_key),
            Err(ProductInvocationError::InvalidRequest)
        );
    }
}
