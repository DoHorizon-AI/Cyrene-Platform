//! Fixed Product HTTP adapter modules for Workspace projections.
//!
//! Owner-specific modules choose Product OpenAPI operations; `http` resolves
//! only injected owner endpoints and performs bounded JSON transport. / 各 owner 模块映射固定契约路由，公共层负责安全传输。

mod catalyst;
mod echo;
mod exchange;
mod http;
mod navigator;
mod reactor;
mod yield_api;

use cy_proto::workspace_v1::WorkspaceProductApiOwner;

use crate::{
    product_projection::authorize_product_invocation, ProductInvocationError,
    ProductInvocationPort, ProductInvocationRequest, ProductInvocationResponse,
    WorkspaceCallerContext,
};

pub use http::{ProductEndpointConfig, ProductHttpClient};

/// HTTP invocation port for the six fixed Product operation mappings.
pub struct ProductHttpApiAdapter {
    client: ProductHttpClient,
}

impl ProductHttpApiAdapter {
    /// Creates an adapter over an injected, privately configured HTTP client.
    pub fn new(client: ProductHttpClient) -> Self {
        Self { client }
    }
}

/// Compatibility name retained for callers that originally composed only
/// Catalyst and Echo mappings.
pub type CatalystEchoProductApiAdapter = ProductHttpApiAdapter;

#[tonic::async_trait]
impl ProductInvocationPort for ProductHttpApiAdapter {
    async fn invoke(
        &self,
        caller: &WorkspaceCallerContext,
        request: ProductInvocationRequest,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        authorize_product_invocation(caller, caller.workspace_id(), &request)?;
        let target = match request.owner {
            WorkspaceProductApiOwner::Catalyst => catalyst::target(&request)?,
            WorkspaceProductApiOwner::Yield => yield_api::target(&request)?,
            WorkspaceProductApiOwner::Reactor => reactor::target(&request)?,
            WorkspaceProductApiOwner::Exchange => exchange::target(&request)?,
            WorkspaceProductApiOwner::Echo => echo::target(&request)?,
            WorkspaceProductApiOwner::Navigator => navigator::target(caller, &request)?,
            _ => return Err(ProductInvocationError::InvalidRequest),
        };
        self.client.send(target, &request).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use cy_proto::workspace_v1::{
        UserIdentityRef, WorkspaceProductApiContentType, WorkspaceProductApiOperation as Operation,
        WorkspaceProductApiOwner as Owner, WorkspaceProductApiRequestKind as Kind,
    };

    use super::*;
    use crate::product_adapters::http::{
        ConfiguredProductEndpointResolver, ProductHttpRequest, ProductHttpResponse,
        ProductHttpTransport,
    };

    struct CountingTransport {
        calls: Arc<AtomicUsize>,
    }

    #[tonic::async_trait]
    impl ProductHttpTransport for CountingTransport {
        async fn send(
            &self,
            _request: ProductHttpRequest,
        ) -> Result<ProductHttpResponse, ProductInvocationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProductHttpResponse::new(
                200,
                WorkspaceProductApiContentType::ApplicationJson,
                b"[]".to_vec(),
            ))
        }
    }

    fn member_caller() -> WorkspaceCallerContext {
        WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            },
            "organization-1",
            "workspace-1",
            BTreeSet::new(),
        )
        .unwrap()
    }

    fn adapter(calls: Arc<AtomicUsize>) -> ProductHttpApiAdapter {
        let resolver = ConfiguredProductEndpointResolver::new(vec![
            ProductEndpointConfig::new(Owner::Catalyst, "https://catalyst.test/", "secret-1"),
            ProductEndpointConfig::new(Owner::Yield, "https://yield.test/", "secret-2"),
            ProductEndpointConfig::new(Owner::Reactor, "https://reactor.test/", "secret-3"),
            ProductEndpointConfig::new(Owner::Exchange, "https://exchange.test/", "secret-4"),
            ProductEndpointConfig::new(Owner::Echo, "https://echo.test/", "secret-5"),
            ProductEndpointConfig::new(Owner::Navigator, "https://navigator.test/", "secret-6"),
        ])
        .unwrap();
        let client = ProductHttpClient::with_transport(
            Arc::new(resolver),
            Arc::new(CountingTransport { calls }),
        );
        ProductHttpApiAdapter::new(client)
    }

    #[tokio::test]
    async fn workspace_member_can_read_catalyst_without_projection_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let response = adapter
            .invoke(
                &member_caller(),
                ProductInvocationRequest {
                    owner: Owner::Catalyst,
                    operation: Operation::WorkspaceProductApiOperation01,
                    kind: Kind::Read,
                    resource_id: None,
                    json_body: Vec::new(),
                    idempotency_key: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(response.json_body, b"[]");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn frontend_commands_remain_denied_before_http_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());

        for (owner, operation, body) in [
            (
                Owner::Catalyst,
                Operation::WorkspaceProductApiOperation02,
                br#"{"name":"dataset"}"#.to_vec(),
            ),
            (
                Owner::Echo,
                Operation::WorkspaceProductApiOperation10,
                br#"{"name":"suite","evaluator":"exact_match.v1","expectedField":"expected","actualField":"actual","threshold":1.0}"#.to_vec(),
            ),
        ] {
            let result = adapter
                .invoke(
                    &member_caller(),
                    ProductInvocationRequest {
                        owner,
                        operation,
                        kind: Kind::Command,
                        resource_id: None,
                        json_body: body,
                        idempotency_key: Some("replay-1".to_string()),
                    },
                )
                .await;
            assert_eq!(result.unwrap_err(), ProductInvocationError::PermissionDenied);
        }

        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn product_reads_route_through_the_combined_owner_adapter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let navigator_snapshot = br#"{"workspaceId":"workspace-1","reads":[{"product":"CATALYST","path":"/api/v1/datasets"}]}"#;
        let requests = [
            ProductInvocationRequest {
                owner: Owner::Yield,
                operation: Operation::WorkspaceProductApiOperation03,
                kind: Kind::Read,
                resource_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
                json_body: Vec::new(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Reactor,
                operation: Operation::WorkspaceProductApiOperation05,
                kind: Kind::Read,
                resource_id: None,
                json_body: Vec::new(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Exchange,
                operation: Operation::WorkspaceProductApiOperation07,
                kind: Kind::Read,
                resource_id: None,
                json_body: Vec::new(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Echo,
                operation: Operation::WorkspaceProductApiOperation09,
                kind: Kind::Read,
                resource_id: Some("22222222-2222-4222-8222-222222222222".to_string()),
                json_body: Vec::new(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Navigator,
                operation: Operation::WorkspaceProductApiOperation11,
                kind: Kind::Read,
                resource_id: None,
                json_body: navigator_snapshot.to_vec(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Navigator,
                operation: Operation::WorkspaceProductApiOperation12,
                kind: Kind::Read,
                resource_id: Some("session-1".to_string()),
                json_body: Vec::new(),
                idempotency_key: None,
            },
        ];

        for request in requests {
            let response = adapter
                .invoke(&member_caller(), request)
                .await
                .expect("member Product read should use the fixed owner router");
            assert_eq!(response.status_code, 200);
        }

        assert_eq!(calls.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn navigator_append_remains_denied_by_the_combined_owner_adapter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let result = adapter
            .invoke(
                &member_caller(),
                ProductInvocationRequest {
                    owner: Owner::Navigator,
                    operation: Operation::WorkspaceProductApiOperation13,
                    kind: Kind::Command,
                    resource_id: Some("session-1".to_string()),
                    json_body: br#"{"events":[]}"#.to_vec(),
                    idempotency_key: Some("append-1".to_string()),
                },
            )
            .await;

        assert_eq!(
            result.unwrap_err(),
            ProductInvocationError::PermissionDenied
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
