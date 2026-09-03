//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 api.rs                                                          │
//! │  Module: cy_workspace_fabric::api                                   │
//! │  Role: Stable frontend-to-Workspace API boundary.                   │
//! │                                                                     │
//! │  模块职责：前端到 Workspace 的稳定 API 边界。                          │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::Arc;

use cy_proto::workspace_v1::{WorkspaceApiRequest, WorkspaceApiResponse};

/// Workspace-owned request handler. Relay implementations only forward it.
#[tonic::async_trait]
pub trait WorkspaceApi: Send + Sync + 'static {
    async fn handle(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse;
}

/// LOCAL connectivity adapter for in-process Navigator/CLI composition.
#[derive(Clone)]
pub struct LocalWorkspaceClient {
    api: Arc<dyn WorkspaceApi>,
}

impl LocalWorkspaceClient {
    pub fn new(api: Arc<dyn WorkspaceApi>) -> Self {
        Self { api }
    }

    pub async fn execute(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
        self.api.handle(request).await
    }
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
        async fn handle(&self, request: WorkspaceApiRequest) -> WorkspaceApiResponse {
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

    #[tokio::test]
    async fn local_frontend_observes_workspace_api_without_execution_access() {
        let client = LocalWorkspaceClient::new(Arc::new(FixtureWorkspaceApi));
        let response = client
            .execute(WorkspaceApiRequest {
                request_id: "request-1".to_string(),
                workspace_id: "workspace-1".to_string(),
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
        let Some(workspace_api_response::Outcome::Operation(operation)) = response.outcome else {
            panic!("expected Workspace operation view");
        };
        assert_eq!(operation.state, WorkspaceOperationState::Running as i32);
        assert_eq!(operation.operation.unwrap().id, "operation-1");
    }
}
