//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 reactor.rs                                                      │
//! │  Module: cy_workspace_fabric::product_adapters::reactor             │
//! │  Role: Fixed Product HTTP mappings for Reactor model imports.       │
//! │                                                                     │
//! │  模块职责：Reactor 模型导入列表与创建的固定 Product HTTP 映射。           │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};

use crate::product_projection::{ProductInvocationError, ProductInvocationRequest};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

/// Maps only Reactor `listModelImports` and `createModelImport` to fixed routes.
///
/// The shared client forwards the configured private bearer and preserves the
/// Product response, which remains the authoritative ModelImport projection.
///
/// # Errors
/// Returns a fixed invocation error for unsupported operations, malformed
/// request metadata, or request fields absent from the OpenAPI route.
pub(super) fn target(
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Reactor {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation05, Kind::Read) => {
            if request.resource_id.is_some()
                || !request.json_body.is_empty()
                || request.idempotency_key.is_some()
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Reactor,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("internal"),
                    ProductHttpPathSegment::Static("workspace"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("model-imports"),
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation06, Kind::Command) => {
            if request.resource_id.is_some() || !is_json_object(&request.json_body) {
                return Err(ProductInvocationError::InvalidRequest);
            }
            if request
                .idempotency_key
                .as_ref()
                .is_some_and(|key| key.chars().count() > 200)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Reactor,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("internal"),
                    ProductHttpPathSegment::Static("workspace"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("model-imports"),
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
    use std::sync::{Arc, Mutex};

    use cy_proto::workspace_v1::{
        WorkspaceProductApiContentType as ContentType, WorkspaceProductApiOperation as Operation,
        WorkspaceProductApiOwner as Owner, WorkspaceProductApiRequestKind as Kind,
    };

    use super::*;
    use crate::product_adapters::http::{
        ConfiguredProductEndpointResolver, ProductEndpointConfig, ProductHttpClient,
        ProductHttpRequest, ProductHttpResponse, ProductHttpTransport, TEST_SERVICE_CREDENTIAL,
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
        let owner = Owner::Reactor;
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            owner,
            "organization-1",
            "workspace-1",
            "https://reactor.test/",
            TEST_SERVICE_CREDENTIAL,
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
            owner: Owner::Reactor,
            operation,
            kind,
            resource_id: resource_id.map(str::to_owned),
            json_body: json_body.to_vec(),
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn list_model_imports_uses_fixed_get_route_without_credentials_in_diagnostics() {
        let body = "[]";
        let (client, transport) = test_client(200, body);
        let request = invocation(
            Operation::WorkspaceProductApiOperation05,
            Kind::Read,
            None,
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
        assert_eq!(observed.owner, Owner::Reactor);
        assert_eq!(observed.method, ProductHttpMethod::Get);
        assert_eq!(
            observed.url,
            "https://reactor.test/internal/workspace/v1/model-imports"
        );
        assert!(observed.body.is_empty());
        assert_eq!(observed.idempotency_key, None);
        assert!(!observed.debug.contains(TEST_SERVICE_CREDENTIAL));
    }

    #[tokio::test]
    async fn create_model_import_forwards_body_and_optional_owner_idempotency_key() {
        let body = r#"{"name":"small-model","servingBindingId":"binding-1","source":{"kind":"HUGGING_FACE","repository":"org/model"},"credentialRef":"private-ref"}"#;
        let response_body = r#"{"id":"33333333-3333-4333-8333-333333333333","state":"VALIDATING"}"#;
        let (client, transport) = test_client(201, response_body);
        let request = invocation(
            Operation::WorkspaceProductApiOperation06,
            Kind::Command,
            None,
            body.as_bytes(),
            Some("reactor-create-1"),
        );

        let response = client
            .send(
                &crate::product_adapters::test_member_caller("organization-1", "workspace-1"),
                target(&request).expect("mapped target"),
                &request,
            )
            .await
            .expect("mock Product response");

        assert_eq!(response.status_code, 201);
        assert_eq!(response.json_body, response_body.as_bytes());
        let observed = transport
            .request
            .lock()
            .expect("request mutex")
            .take()
            .expect("request was dispatched");
        assert_eq!(observed.owner, Owner::Reactor);
        assert_eq!(observed.method, ProductHttpMethod::Post);
        assert_eq!(
            observed.url,
            "https://reactor.test/internal/workspace/v1/model-imports"
        );
        assert_eq!(observed.body, body.as_bytes());
        assert_eq!(
            observed.idempotency_key.as_deref(),
            Some("reactor-create-1")
        );
        assert!(!observed.debug.contains(TEST_SERVICE_CREDENTIAL));
        assert!(!observed.debug.contains("private-ref"));
    }

    #[tokio::test]
    async fn create_model_import_does_not_require_an_idempotency_key() {
        let body = r#"{"name":"small-model","servingBindingId":"binding-1","source":{"kind":"HUGGING_FACE","repository":"org/model"}}"#;
        let (client, transport) = test_client(
            201,
            r#"{"id":"33333333-3333-4333-8333-333333333333","state":"VALIDATING"}"#,
        );
        let request = invocation(
            Operation::WorkspaceProductApiOperation06,
            Kind::Command,
            None,
            body.as_bytes(),
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

        assert_eq!(response.status_code, 201);
        let observed = transport
            .request
            .lock()
            .expect("request mutex")
            .take()
            .expect("request was dispatched");
        assert_eq!(observed.idempotency_key, None);
    }

    #[test]
    fn list_rejects_unmapped_fields_and_create_rejects_non_object_body() {
        let list_with_key = invocation(
            Operation::WorkspaceProductApiOperation05,
            Kind::Read,
            None,
            &[],
            Some("unsupported-key"),
        );
        assert_eq!(
            target(&list_with_key),
            Err(ProductInvocationError::InvalidRequest)
        );

        let create_with_array_body = invocation(
            Operation::WorkspaceProductApiOperation06,
            Kind::Command,
            None,
            b"[]",
            None,
        );
        assert_eq!(
            target(&create_with_array_body),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[test]
    fn legacy_global_route_is_not_an_allowed_reactor_target() {
        assert_eq!(
            ProductHttpTarget::new(
                Owner::Reactor,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("model-imports"),
                ],
            ),
            Err(ProductInvocationError::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn serving_binding_forbidden_response_remains_forbidden() {
        let (client, _) = test_client(403, r#"{"detail":"serving binding is not granted"}"#);
        let request = invocation(
            Operation::WorkspaceProductApiOperation06,
            Kind::Command,
            None,
            br#"{"name":"small-model","servingBindingId":"binding-1","source":{"kind":"HUGGING_FACE","repository":"org/model"}}"#,
            None,
        );

        let response = client
            .send(
                &crate::product_adapters::test_member_caller("organization-1", "workspace-1"),
                target(&request).expect("mapped private route"),
                &request,
            )
            .await
            .expect("owner authorization response remains a Product response");

        assert_eq!(response.status_code, 403);
        assert_eq!(
            response.json_body,
            br#"{"detail":"serving binding is not granted"}"#
        );
    }
}
