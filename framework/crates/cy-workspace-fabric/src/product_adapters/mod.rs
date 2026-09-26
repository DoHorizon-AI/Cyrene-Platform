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

#[cfg(test)]
pub(crate) fn test_member_caller(
    organization_id: &str,
    workspace_id: &str,
) -> WorkspaceCallerContext {
    use std::collections::BTreeSet;

    use cy_proto::workspace_v1::UserIdentityRef;

    WorkspaceCallerContext::user_member(
        UserIdentityRef {
            issuer: "https://identity.test".to_string(),
            subject: "user-1".to_string(),
        },
        organization_id,
        workspace_id,
        BTreeSet::new(),
    )
    .unwrap()
}

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
        // These owners still map to legacy global Product routes. Keep every
        // operation unavailable at the dispatch boundary until its private
        // route and scope-bound service credential are integrated. Endpoint
        // configuration alone must never re-enable a legacy request.
        if matches!(
            request.owner,
            WorkspaceProductApiOwner::Reactor | WorkspaceProductApiOwner::Navigator
        ) {
            return Err(ProductInvocationError::Unavailable);
        }

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
        self.client.send(caller, target, &request).await
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

    fn exchange_writer_caller() -> WorkspaceCallerContext {
        let mut roles = BTreeSet::new();
        roles.insert("workspace.product.command.exchange.create_route_draft.v1".to_string());
        WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            },
            "organization-1",
            "workspace-1",
            roles,
        )
        .unwrap()
    }

    fn adapter(calls: Arc<AtomicUsize>) -> ProductHttpApiAdapter {
        let resolver = ConfiguredProductEndpointResolver::new(vec![
            ProductEndpointConfig::new(
                Owner::Catalyst,
                "organization-1",
                "workspace-1",
                "https://catalyst.test/",
                "secret-1",
            ),
            ProductEndpointConfig::new(
                Owner::Yield,
                "organization-1",
                "workspace-1",
                "https://yield.test/",
                "secret-2",
            ),
            ProductEndpointConfig::new(
                Owner::Reactor,
                "organization-1",
                "workspace-1",
                "https://reactor.test/",
                "secret-3",
            ),
            ProductEndpointConfig::new(
                Owner::Exchange,
                "organization-1",
                "workspace-1",
                "https://exchange.test/",
                "secret-4",
            ),
            ProductEndpointConfig::new(
                Owner::Echo,
                "organization-1",
                "workspace-1",
                "https://echo.test/",
                "secret-5",
            ),
            ProductEndpointConfig::new(
                Owner::Navigator,
                "organization-1",
                "workspace-1",
                "https://navigator.test/",
                "secret-6",
            ),
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
            (
                Owner::Exchange,
                Operation::WorkspaceProductApiOperation08,
                br#"{"endpointId":"endpoint-1"}"#.to_vec(),
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
    async fn enabled_private_product_reads_route_through_the_combined_owner_adapter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
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
        ];

        for request in requests {
            let response = adapter
                .invoke(&member_caller(), request)
                .await
                .expect("member Product read should use the fixed owner router");
            assert_eq!(response.status_code, 200);
        }

        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exchange_command_uses_the_scoped_owner_adapter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let response = adapter
            .invoke(
                &exchange_writer_caller(),
                ProductInvocationRequest {
                    owner: Owner::Exchange,
                    operation: Operation::WorkspaceProductApiOperation08,
                    kind: Kind::Command,
                    resource_id: None,
                    json_body: br#"{"endpointId":"endpoint-1"}"#.to_vec(),
                    idempotency_key: Some("exchange-command-1".to_string()),
                },
            )
            .await
            .expect("the exact Exchange role may reach its private route");

        assert_eq!(response.status_code, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reactor_and_navigator_operations_remain_unavailable_with_configured_endpoints() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let requests = [
            ProductInvocationRequest {
                owner: Owner::Reactor,
                operation: Operation::WorkspaceProductApiOperation05,
                kind: Kind::Read,
                resource_id: None,
                json_body: Vec::new(),
                idempotency_key: None,
            },
            ProductInvocationRequest {
                owner: Owner::Reactor,
                operation: Operation::WorkspaceProductApiOperation06,
                kind: Kind::Command,
                resource_id: None,
                json_body: br#"{"model":"test"}"#.to_vec(),
                idempotency_key: Some("reactor-command-1".to_string()),
            },
            ProductInvocationRequest {
                owner: Owner::Navigator,
                operation: Operation::WorkspaceProductApiOperation11,
                kind: Kind::Read,
                resource_id: None,
                json_body: br#"{"workspaceId":"workspace-1","reads":[{"product":"CATALYST","path":"/api/v1/datasets"}]}"#.to_vec(),
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
            ProductInvocationRequest {
                owner: Owner::Navigator,
                operation: Operation::WorkspaceProductApiOperation13,
                kind: Kind::Command,
                resource_id: Some("session-1".to_string()),
                json_body: br#"{"events":[]}"#.to_vec(),
                idempotency_key: Some("append-1".to_string()),
            },
        ];

        for request in requests {
            assert_eq!(
                adapter.invoke(&member_caller(), request).await.unwrap_err(),
                ProductInvocationError::Unavailable
            );
        }

        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn org_or_workspace_scope_mismatch_fails_closed_for_all_six_owners() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = adapter(calls.clone());
        let requests = [
            ProductInvocationRequest {
                owner: Owner::Catalyst,
                operation: Operation::WorkspaceProductApiOperation01,
                kind: Kind::Read,
                resource_id: None,
                json_body: Vec::new(),
                idempotency_key: None,
            },
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
                operation: Operation::WorkspaceProductApiOperation12,
                kind: Kind::Read,
                resource_id: Some("session-1".to_string()),
                json_body: Vec::new(),
                idempotency_key: None,
            },
        ];
        let wrong_scopes = [
            crate::product_adapters::test_member_caller("organization-1", "workspace-2"),
            crate::product_adapters::test_member_caller("organization-2", "workspace-1"),
        ];

        for caller in &wrong_scopes {
            for request in requests.iter().cloned() {
                assert_eq!(
                    adapter.invoke(caller, request).await.unwrap_err(),
                    ProductInvocationError::Unavailable
                );
            }
        }

        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_private_endpoint_configuration_fails_closed() {
        let client = ProductHttpClient::from_private_config(Vec::new()).unwrap();
        let adapter = ProductHttpApiAdapter::new(client);
        for (owner, operation) in [
            (Owner::Catalyst, Operation::WorkspaceProductApiOperation01),
            (Owner::Exchange, Operation::WorkspaceProductApiOperation07),
        ] {
            let result = adapter
                .invoke(
                    &member_caller(),
                    ProductInvocationRequest {
                        owner,
                        operation,
                        kind: Kind::Read,
                        resource_id: None,
                        json_body: Vec::new(),
                        idempotency_key: None,
                    },
                )
                .await;

            assert_eq!(result.unwrap_err(), ProductInvocationError::Unavailable);
        }
    }

    #[tokio::test]
    async fn exchange_endpoint_with_mismatched_configured_scope_fails_before_transport() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = ConfiguredProductEndpointResolver::new(vec![ProductEndpointConfig::new(
            Owner::Exchange,
            "organization-2",
            "workspace-1",
            "https://exchange.test/",
            "private-test-credential",
        )])
        .unwrap();
        let client = ProductHttpClient::with_transport(
            Arc::new(resolver),
            Arc::new(CountingTransport {
                calls: calls.clone(),
            }),
        );
        let adapter = ProductHttpApiAdapter::new(client);
        let result = adapter
            .invoke(
                &member_caller(),
                ProductInvocationRequest {
                    owner: Owner::Exchange,
                    operation: Operation::WorkspaceProductApiOperation07,
                    kind: Kind::Read,
                    resource_id: None,
                    json_body: Vec::new(),
                    idempotency_key: None,
                },
            )
            .await;

        assert_eq!(result.unwrap_err(), ProductInvocationError::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
