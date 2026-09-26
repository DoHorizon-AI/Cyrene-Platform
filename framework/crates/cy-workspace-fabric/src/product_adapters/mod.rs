//! Fixed Product HTTP adapter modules for Workspace projections.
//!
//! Owner-specific modules choose Product OpenAPI operations; `http` resolves
//! only injected owner endpoints and performs bounded JSON transport. / 各 owner 模块映射固定契约路由，公共层负责安全传输。

mod catalyst;
mod echo;
mod http;

use cy_proto::workspace_v1::WorkspaceProductApiOwner;

use crate::{
    product_projection::authorize_product_invocation, ProductInvocationError,
    ProductInvocationPort, ProductInvocationRequest, ProductInvocationResponse,
    WorkspaceCallerContext,
};

pub use http::{ProductEndpointConfig, ProductHttpClient};

/// HTTP invocation port for the Catalyst and Echo Product operation mappings.
pub struct CatalystEchoProductApiAdapter {
    client: ProductHttpClient,
}

impl CatalystEchoProductApiAdapter {
    /// Creates an adapter over an injected, privately configured HTTP client.
    pub fn new(client: ProductHttpClient) -> Self {
        Self { client }
    }
}

#[tonic::async_trait]
impl ProductInvocationPort for CatalystEchoProductApiAdapter {
    async fn invoke(
        &self,
        caller: &WorkspaceCallerContext,
        request: ProductInvocationRequest,
    ) -> Result<ProductInvocationResponse, ProductInvocationError> {
        authorize_product_invocation(caller, caller.workspace_id(), &request)?;
        let target = match request.owner {
            WorkspaceProductApiOwner::Catalyst => catalyst::target(&request)?,
            WorkspaceProductApiOwner::Echo => echo::target(&request)?,
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

    fn adapter(calls: Arc<AtomicUsize>) -> CatalystEchoProductApiAdapter {
        let resolver = ConfiguredProductEndpointResolver::new(vec![
            ProductEndpointConfig::new(Owner::Catalyst, "https://catalyst.test/", "secret-1"),
            ProductEndpointConfig::new(Owner::Echo, "https://echo.test/", "secret-2"),
        ])
        .unwrap();
        let client = ProductHttpClient::with_transport(
            Arc::new(resolver),
            Arc::new(CountingTransport { calls }),
        );
        CatalystEchoProductApiAdapter::new(client)
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
}
