//! Exchange Product HTTP route mappings.
//!
//! Exchange remains the authority for gateway routes and draft state. This
//! module maps only the closed Exchange operation keys to Product OpenAPI v1.

use cy_proto::workspace_v1::{
    WorkspaceProductApiOperation as Operation, WorkspaceProductApiOwner as Owner,
    WorkspaceProductApiRequestKind as Kind,
};

use crate::{ProductInvocationError, ProductInvocationRequest};

use super::http::{ProductHttpMethod, ProductHttpPathSegment, ProductHttpTarget};

const MAX_IDEMPOTENCY_KEY_CHARS: usize = 200;

/// Maps Exchange `listGatewayRoutes` and `create_draft` to fixed Product paths.
pub(super) fn target(
    request: &ProductInvocationRequest,
) -> Result<ProductHttpTarget, ProductInvocationError> {
    if request.owner != Owner::Exchange {
        return Err(ProductInvocationError::InvalidRequest);
    }

    match (request.operation, request.kind) {
        (Operation::WorkspaceProductApiOperation07, Kind::Read) => {
            if request.resource_id.is_some()
                || !request.json_body.is_empty()
                || request.idempotency_key.is_some()
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Exchange,
                ProductHttpMethod::Get,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("gateway-routes"),
                ],
            )
        }
        (Operation::WorkspaceProductApiOperation08, Kind::Command) => {
            if request.resource_id.is_some()
                || request.json_body.is_empty()
                || !is_json_object(&request.json_body)
                || !request
                    .idempotency_key
                    .as_deref()
                    .is_some_and(valid_idempotency_key)
            {
                return Err(ProductInvocationError::InvalidRequest);
            }
            ProductHttpTarget::new(
                Owner::Exchange,
                ProductHttpMethod::Post,
                vec![
                    ProductHttpPathSegment::Static("api"),
                    ProductHttpPathSegment::Static("v1"),
                    ProductHttpPathSegment::Static("gateway-route-drafts"),
                ],
            )
        }
        _ => Err(ProductInvocationError::InvalidRequest),
    }
}

fn valid_idempotency_key(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_IDEMPOTENCY_KEY_CHARS
        && value
            .chars()
            .all(|character| character.is_ascii() && !character.is_ascii_control())
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
        ConfiguredProductEndpointResolver, ProductHttpRequest, ProductHttpResponse,
        ProductHttpTransport,
    };
    use crate::product_adapters::{ProductEndpointConfig, ProductHttpClient};

    struct MockTransport {
        requests: Arc<Mutex<Vec<ProductHttpRequest>>>,
        response: Mutex<Option<ProductHttpResponse>>,
    }

    #[tonic::async_trait]
    impl ProductHttpTransport for MockTransport {
        async fn send(
            &self,
            request: ProductHttpRequest,
        ) -> Result<ProductHttpResponse, ProductInvocationError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or(ProductInvocationError::Unavailable)
        }
    }

    fn mock_client(
        status_code: u16,
        response_body: &[u8],
    ) -> (ProductHttpClient, Arc<Mutex<Vec<ProductHttpRequest>>>) {
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            Owner::Exchange,
            "https://exchange.test/",
            "private-test-credential",
        )])
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = ProductHttpClient::with_transport(
            Arc::new(resolver),
            Arc::new(MockTransport {
                requests: requests.clone(),
                response: Mutex::new(Some(ProductHttpResponse::new(
                    status_code,
                    ContentType::ApplicationJson,
                    response_body.to_vec(),
                ))),
            }),
        );
        (client, requests)
    }

    fn invocation(
        operation: Operation,
        kind: Kind,
        json_body: Vec<u8>,
        idempotency_key: Option<&str>,
    ) -> ProductInvocationRequest {
        ProductInvocationRequest {
            owner: Owner::Exchange,
            operation,
            kind,
            resource_id: None,
            json_body,
            idempotency_key: idempotency_key.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn list_gateway_routes_uses_only_the_fixed_read_route() {
        let (client, requests) = mock_client(200, b"[]");
        let request = invocation(
            Operation::WorkspaceProductApiOperation07,
            Kind::Read,
            Vec::new(),
            None,
        );
        let response = client
            .send(target(&request).unwrap(), &request)
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(response.json_body, b"[]");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].owner(), Owner::Exchange);
        assert_eq!(requests[0].method(), ProductHttpMethod::Get);
        assert_eq!(
            requests[0].url().as_str(),
            "https://exchange.test/api/v1/gateway-routes"
        );
        assert!(requests[0].body().is_empty());
        assert_eq!(requests[0].idempotency_key(), None);
    }

    #[tokio::test]
    async fn create_draft_forwards_the_required_key_and_preserves_draft_response() {
        let response_body = br#"{"id":"route-1","status":"DRAFT","resourceVersion":1}"#;
        let (client, requests) = mock_client(201, response_body);
        let request_body = br#"{"endpointId":"endpoint-1","modelPattern":"*","targetBindingId":"binding-1","targetModel":"model-1","priority":1,"source":{"product":"reactor","resourceUri":"cyrene://endpoint/1","resourceVersion":"v1","artifactDigest":"sha256:abc"}}"#;
        let request = invocation(
            Operation::WorkspaceProductApiOperation08,
            Kind::Command,
            request_body.to_vec(),
            Some("draft-replay-1"),
        );

        let response = client
            .send(target(&request).unwrap(), &request)
            .await
            .unwrap();

        assert_eq!(response.status_code, 201);
        assert_eq!(response.json_body, response_body);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), ProductHttpMethod::Post);
        assert_eq!(
            requests[0].url().as_str(),
            "https://exchange.test/api/v1/gateway-route-drafts"
        );
        assert_eq!(requests[0].body(), request_body);
        assert_eq!(requests[0].idempotency_key(), Some("draft-replay-1"));
        assert!(String::from_utf8_lossy(&response.json_body).contains("DRAFT"));
    }

    #[test]
    fn create_draft_without_valid_idempotency_key_fails_before_transport() {
        let body = br#"{"endpointId":"endpoint-1"}"#.to_vec();
        for key in [None, Some("  "), Some("bad\nkey")] {
            let request = invocation(
                Operation::WorkspaceProductApiOperation08,
                Kind::Command,
                body.clone(),
                key,
            );
            assert_eq!(
                target(&request),
                Err(ProductInvocationError::InvalidRequest)
            );
        }
    }

    #[test]
    fn exchange_rejects_unmapped_operations_and_method_kind_mismatches() {
        let request = invocation(
            Operation::WorkspaceProductApiOperation07,
            Kind::Command,
            br#"{}"#.to_vec(),
            Some("key-1"),
        );
        assert_eq!(
            target(&request),
            Err(ProductInvocationError::InvalidRequest)
        );
    }
}
