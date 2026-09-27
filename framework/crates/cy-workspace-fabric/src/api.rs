//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 api.rs                                                          │
//! │  Module: cy_workspace_fabric::api                                   │
//! │  Role: Stable frontend-to-Workspace API boundary.                   │
//! │                                                                     │
//! │  模块职责：前端到 Workspace 的稳定 API 边界。                          │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;

use cy_observability::TraceContext;
use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::workspace_v1::workspace_direct_service_server::{
    WorkspaceDirectService, WorkspaceDirectServiceServer,
};
use cy_proto::workspace_v1::workspace_relay_service_server::{
    WorkspaceRelayService, WorkspaceRelayServiceServer,
};
use cy_proto::workspace_v1::{WorkspaceApiRequest, WorkspaceApiResponse};

use crate::WorkspaceCallerContext;

/// Builds a Relay gRPC server with enough room for a maximum Product JSON body
/// and its enclosing protobuf/context fields.
pub fn bounded_workspace_relay_server<T: WorkspaceRelayService>(
    service: T,
) -> WorkspaceRelayServiceServer<T> {
    WorkspaceRelayServiceServer::new(service)
        .max_decoding_message_size(crate::WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
        .max_encoding_message_size(crate::WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
}

/// Builds a Direct gRPC server with enough room for a maximum Product JSON body
/// and its enclosing protobuf/authentication fields.
pub fn bounded_workspace_direct_server<T: WorkspaceDirectService>(
    service: T,
) -> WorkspaceDirectServiceServer<T> {
    WorkspaceDirectServiceServer::new(service)
        .max_decoding_message_size(crate::WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
        .max_encoding_message_size(crate::WORKSPACE_API_GRPC_MESSAGE_MAX_BYTES)
}

/// Workspace-owned request handler. Relay implementations only forward it.
#[tonic::async_trait]
pub trait WorkspaceApi: Send + Sync + 'static {
    /// Legacy unauthenticated entry point. It always fails closed.
    async fn handle(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
        unauthenticated_response(request.request_id)
    }

    /// Handles a request after a trusted transport supplied server-derived context.
    /// Implementations must apply operation-specific authorization before dispatch.
    async fn handle_authenticated(
        &self,
        request: WorkspaceApiRequest,
        _caller: WorkspaceCallerContext,
    ) -> WorkspaceApiResponse {
        unauthenticated_response(request.request_id)
    }
}

/// LOCAL connectivity adapter for in-process client and CLI composition.
#[derive(Clone)]
pub struct LocalWorkspaceClient {
    api: Arc<dyn WorkspaceApi>,
}

impl LocalWorkspaceClient {
    pub fn new(api: Arc<dyn WorkspaceApi>) -> Self {
        Self { api }
    }

    pub async fn execute(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
        dispatch_workspace_request(self.api.as_ref(), request).await
    }
}

/// Validates trace context before dispatching to the Workspace authority.
/// Invalid or missing context is removed without recording the raw value.
/// 校验追踪上下文后再派发给 Workspace 权威；无效或缺失上下文不会记录原始值。
pub(crate) async fn dispatch_workspace_request(
    api: &dyn WorkspaceApi,
    mut request: WorkspaceApiRequest,
) -> WorkspaceApiResponse {
    sanitize_traceparent(&mut request);
    api.handle(request).await
}

/// Validates trace context and dispatches with a caller established by a trusted boundary.
pub(crate) async fn dispatch_authenticated_workspace_request(
    api: &dyn WorkspaceApi,
    mut request: WorkspaceApiRequest,
    caller: WorkspaceCallerContext,
) -> WorkspaceApiResponse {
    sanitize_traceparent(&mut request);
    api.handle_authenticated(request, caller).await
}

fn sanitize_traceparent(request: &mut WorkspaceApiRequest) {
    let trace_context = TraceContext::parse_traceparent(&request.traceparent).ok();
    if let Some(context) = trace_context {
        request.traceparent = context.to_traceparent();
        tracing::info!(
            event.name = "platform.workspace.api.request",
            trace_id = %context.trace_id_hex(),
            span_id = %context.span_id_hex(),
            message = "Workspace API request reached its authority handler",
        );
    } else {
        // Untrusted invalid input is discarded without exposing its value.
        // 无效的非可信输入会被丢弃，且不记录其内容。
        request.traceparent.clear();
    }
}

fn unauthenticated_response(request_id: String) -> WorkspaceApiResponse {
    WorkspaceApiResponse {
        request_id,
        outcome: Some(
            cy_proto::workspace_v1::workspace_api_response::Outcome::Error(RpcStatus {
                code: 16,
                message: "WORKSPACE_CALLER_CONTEXT_REQUIRED".to_string(),
                details: Vec::new(),
            }),
        ),
    }
}

pub(crate) fn unauthenticated_workspace_response(request_id: String) -> WorkspaceApiResponse {
    unauthenticated_response(request_id)
}

#[cfg(test)]
mod tests {
    use cy_proto::semantic_v1::Identity;
    use cy_proto::workspace_v1::{
        workspace_api_request, workspace_api_response, GetWorkspaceOperationRequest,
        WorkspaceOperationState, WorkspaceOperationView,
    };

    use super::*;

    struct FixtureWorkspaceApi;

    #[tonic::async_trait]
    impl WorkspaceApi for FixtureWorkspaceApi {
        async fn handle_authenticated(
            &self,
            request: WorkspaceApiRequest,
            _caller: WorkspaceCallerContext,
        ) -> WorkspaceApiResponse {
            let operation = match request.request {
                Some(workspace_api_request::Request::GetOperation(value)) => value.operation,
                _ => None,
            };
            WorkspaceApiResponse {
                request_id: request.request_id,
                outcome: Some(workspace_api_response::Outcome::Operation(
                    WorkspaceOperationView {
                        operation,
                        state: WorkspaceOperationState::Running as i32,
                        completed_units: 1,
                        total_units: 10,
                        unit: "steps".to_string(),
                        artifact_uris: Vec::new(),
                        resource_references: Vec::new(),
                        status_reason: "fixture".to_string(),
                        authority_instance_id: "workspace-authority-1".to_string(),
                    },
                )),
            }
        }
    }

    struct CapturingApi(Arc<std::sync::Mutex<Option<WorkspaceApiRequest>>>);

    #[tonic::async_trait]
    impl WorkspaceApi for CapturingApi {
        async fn handle_authenticated(
            &self,
            request: WorkspaceApiRequest,
            _caller: WorkspaceCallerContext,
        ) -> WorkspaceApiResponse {
            *self.0.lock().unwrap() = Some(request.clone());
            WorkspaceApiResponse {
                request_id: request.request_id,
                outcome: None,
            }
        }
    }

    fn traced_request(traceparent: &str) -> WorkspaceApiRequest {
        WorkspaceApiRequest {
            request_id: "request-trace-test".to_string(),
            workspace_id: "workspace-trace-test".to_string(),
            traceparent: traceparent.to_string(),
            request: None,
        }
    }

    fn caller() -> WorkspaceCallerContext {
        WorkspaceCallerContext::user_member(
            cy_proto::workspace_v1::UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            },
            "organization-1",
            "workspace-trace-test",
            std::collections::BTreeSet::new(),
        )
        .expect("valid verified caller")
    }

    #[tokio::test]
    async fn local_frontend_observes_workspace_api_without_execution_access() {
        let client = LocalWorkspaceClient::new(Arc::new(FixtureWorkspaceApi));
        let response = client
            .execute(WorkspaceApiRequest {
                request_id: "request-1".to_string(),
                workspace_id: "workspace-1".to_string(),
                traceparent: String::new(),
                request: Some(workspace_api_request::Request::GetOperation(
                    GetWorkspaceOperationRequest {
                        operation: Some(Identity {
                            id: "operation-1".to_string(),
                            generation: 1,
                        }),
                    },
                )),
            })
            .await;
        let Some(workspace_api_response::Outcome::Error(error)) = response.outcome else {
            panic!("expected an unauthenticated response");
        };
        assert_eq!(error.code, 16);
        assert_eq!(error.message, "WORKSPACE_CALLER_CONTEXT_REQUIRED");
    }

    #[tokio::test]
    async fn explicit_authenticated_dispatch_preserves_fixture_handler_path() {
        let api = FixtureWorkspaceApi;
        let response = dispatch_authenticated_workspace_request(
            &api,
            WorkspaceApiRequest {
                request_id: "request-1".to_string(),
                workspace_id: "workspace-trace-test".to_string(),
                traceparent: String::new(),
                request: Some(workspace_api_request::Request::GetOperation(
                    GetWorkspaceOperationRequest {
                        operation: Some(Identity {
                            id: "operation-1".to_string(),
                            generation: 1,
                        }),
                    },
                )),
            },
            caller(),
        )
        .await;
        let Some(workspace_api_response::Outcome::Operation(operation)) = response.outcome else {
            panic!("expected Workspace operation view");
        };
        assert_eq!(operation.state, WorkspaceOperationState::Running as i32);
        assert_eq!(operation.operation.unwrap().id, "operation-1");
    }

    #[tokio::test]
    async fn valid_traceparent_reaches_handler_as_canonical_w3c_context() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let api = CapturingApi(captured.clone());
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

        dispatch_authenticated_workspace_request(&api, traced_request(traceparent), caller()).await;

        let captured = captured.lock().unwrap();
        assert_eq!(captured.as_ref().unwrap().traceparent, traceparent);
        let context = TraceContext::parse_traceparent(&captured.as_ref().unwrap().traceparent)
            .expect("handler context should remain valid");
        assert_eq!(context.trace_id_hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
    }

    #[tokio::test]
    async fn invalid_traceparent_is_removed_before_handler() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let api = CapturingApi(captured.clone());
        let untrusted = "Bearer sensitive-session-value";

        dispatch_authenticated_workspace_request(&api, traced_request(untrusted), caller()).await;

        assert!(captured
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .traceparent
            .is_empty());
    }

    #[tokio::test]
    async fn missing_traceparent_does_not_create_a_trace_context() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let api = CapturingApi(captured.clone());

        dispatch_authenticated_workspace_request(&api, traced_request(""), caller()).await;

        assert!(captured
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .traceparent
            .is_empty());
    }
}
