//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 control_plane.rs                                                 │
//! │  Module: cy_workspace_fabric::control_plane                         │
//! │  Role: Workspace authority checks and product-neutral API dispatch. │
//! │                                                                      │
//! │  模块职责：校验 Workspace 权威边界并分发产品中立 API 请求。              │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This handler assumes its caller has already authenticated the frontend and
//! checked organization membership, as `DirectWorkspaceServer` and
//! `WorkspaceRelay` do. Do not expose it as an unauthenticated endpoint. The
//! dispatcher is an integration port; this module provides neither production
//! IAM nor Product persistence.

use std::sync::Arc;

use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::semantic_v1::Identity;
use cy_proto::workspace_v1::{
    workspace_api_request, workspace_api_response, GetWorkspaceOperationRequest,
    StartWorkspaceOperationRequest, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceOperationView,
};
use thiserror::Error;

use crate::{WorkspaceApi, WorkspaceAuthorizationError, WorkspaceCallerContext};

/// A product-neutral request that a composition root can route to a Product
/// API adapter without transferring Workspace identity ownership.
#[derive(Debug, Clone)]
pub enum WorkspaceProductRequest {
    /// Start the operation identified by the shared semantic contract.
    StartOperation(StartWorkspaceOperationRequest),
    /// Read an operation projection by its shared semantic identity.
    GetOperation(GetWorkspaceOperationRequest),
}

/// A Product API projection returned to the Workspace Control Plane.
///
/// The dispatcher must identify the Workspace whose Product API answered. The
/// control plane checks that identity before returning the operation view.
#[derive(Debug, Clone)]
pub struct WorkspaceOperationProjection {
    /// Workspace identity asserted by the Product API adapter.
    pub workspace_id: String,
    /// Product-neutral operation view defined by the existing Workspace API.
    pub operation: WorkspaceOperationView,
}

/// Product API dispatch boundary used by the Workspace Control Plane.
///
/// Implementations route requests to the appropriate Product-owned API and
/// return its projection. They must not rely on this port for user
/// authentication, membership persistence, or Workspace authority storage.
#[tonic::async_trait]
pub trait WorkspaceRequestDispatcher: Send + Sync + 'static {
    /// Dispatches one validated request for the Workspace owned by this
    /// control-plane instance.
    async fn dispatch(
        &self,
        workspace_id: &str,
        request: WorkspaceProductRequest,
    ) -> Result<WorkspaceOperationProjection, WorkspaceDispatchError>;
}

/// Stable, product-neutral failures that the dispatcher can return.
///
/// The API response uses fixed messages for these variants so adapter internals
/// and credentials cannot leak through an error response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkspaceDispatchError {
    /// The routed Product API rejected a malformed request.
    #[error("WORKSPACE_PRODUCT_REQUEST_INVALID")]
    InvalidRequest,
    /// The routed Product API denied the request.
    #[error("WORKSPACE_PRODUCT_REQUEST_DENIED")]
    PermissionDenied,
    /// The requested Product-owned resource does not exist.
    #[error("WORKSPACE_RESOURCE_NOT_FOUND")]
    NotFound,
    /// The request cannot be applied in the current Product state.
    #[error("WORKSPACE_REQUEST_FAILED_PRECONDITION")]
    FailedPrecondition,
    /// The Product API is temporarily unavailable.
    #[error("WORKSPACE_PRODUCT_UNAVAILABLE")]
    Unavailable,
    /// The Product API failed without a safe public explanation.
    #[error("WORKSPACE_PRODUCT_FAILURE")]
    Internal,
}

/// Invalid immutable authority configuration for a Workspace Control Plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkspaceControlPlaneConfigError {
    /// A concrete Workspace identity is required for authority checks.
    #[error("workspace_id must not be empty")]
    EmptyWorkspaceId,
    /// An instance marker is required for API projections.
    #[error("authority_instance_id must not be empty")]
    EmptyAuthorityInstanceId,
}

/// Application-level Workspace API handler.
///
/// The configured Workspace identity pins authority locally. Product requests
/// are forwarded through `WorkspaceRequestDispatcher`; this type owns no
/// Product records or membership database.
pub struct WorkspaceControlPlane {
    workspace_id: String,
    authority_instance_id: String,
    dispatcher: Arc<dyn WorkspaceRequestDispatcher>,
}

impl WorkspaceControlPlane {
    /// Creates a handler pinned to one Workspace and one authority lifetime.
    ///
    /// # Errors
    /// Returns an error if either identity is empty after trimming whitespace.
    pub fn new(
        workspace_id: impl Into<String>,
        authority_instance_id: impl Into<String>,
        dispatcher: Arc<dyn WorkspaceRequestDispatcher>,
    ) -> Result<Self, WorkspaceControlPlaneConfigError> {
        let workspace_id = workspace_id.into();
        if workspace_id.trim().is_empty() {
            return Err(WorkspaceControlPlaneConfigError::EmptyWorkspaceId);
        }

        let authority_instance_id = authority_instance_id.into();
        if authority_instance_id.trim().is_empty() {
            return Err(WorkspaceControlPlaneConfigError::EmptyAuthorityInstanceId);
        }

        Ok(Self {
            workspace_id,
            authority_instance_id,
            dispatcher,
        })
    }

    fn error_response(
        request_id: String,
        code: i32,
        message: &'static str,
    ) -> WorkspaceApiResponse {
        WorkspaceApiResponse {
            request_id,
            outcome: Some(workspace_api_response::Outcome::Error(RpcStatus {
                code,
                message: message.to_string(),
                details: Vec::new(),
            })),
        }
    }

    fn dispatch_error_response(
        request_id: String,
        error: WorkspaceDispatchError,
    ) -> WorkspaceApiResponse {
        let (code, message) = match error {
            WorkspaceDispatchError::InvalidRequest => (3, "WORKSPACE_PRODUCT_REQUEST_INVALID"),
            WorkspaceDispatchError::PermissionDenied => (7, "WORKSPACE_PRODUCT_REQUEST_DENIED"),
            WorkspaceDispatchError::NotFound => (5, "WORKSPACE_RESOURCE_NOT_FOUND"),
            WorkspaceDispatchError::FailedPrecondition => {
                (9, "WORKSPACE_REQUEST_FAILED_PRECONDITION")
            }
            WorkspaceDispatchError::Unavailable => (14, "WORKSPACE_PRODUCT_UNAVAILABLE"),
            WorkspaceDispatchError::Internal => (13, "WORKSPACE_PRODUCT_FAILURE"),
        };
        Self::error_response(request_id, code, message)
    }

    fn validated_product_request(
        request: workspace_api_request::Request,
    ) -> Result<(WorkspaceProductRequest, Identity), &'static str> {
        let (product_request, operation) = match request {
            workspace_api_request::Request::StartOperation(request) => {
                let operation = request
                    .operation
                    .clone()
                    .ok_or("WORKSPACE_OPERATION_IDENTITY_REQUIRED")?;
                (WorkspaceProductRequest::StartOperation(request), operation)
            }
            workspace_api_request::Request::GetOperation(request) => {
                let operation = request
                    .operation
                    .clone()
                    .ok_or("WORKSPACE_OPERATION_IDENTITY_REQUIRED")?;
                (WorkspaceProductRequest::GetOperation(request), operation)
            }
            #[allow(unreachable_patterns)]
            // Future oneof variants remain closed until the dispatcher adds an explicit mapping.
            _ => return Err("WORKSPACE_REQUEST_UNSUPPORTED"),
        };

        if operation.id.trim().is_empty() || operation.generation == 0 {
            return Err("WORKSPACE_OPERATION_IDENTITY_INVALID");
        }

        Ok((product_request, operation))
    }

    fn validate_projection(
        &self,
        projection: WorkspaceOperationProjection,
        requested_operation: &Identity,
    ) -> Result<WorkspaceOperationView, &'static str> {
        if projection.workspace_id != self.workspace_id {
            return Err("WORKSPACE_PRODUCT_PROJECTION_AUTHORITY_MISMATCH");
        }

        if projection.operation.operation.as_ref() != Some(requested_operation) {
            return Err("WORKSPACE_PRODUCT_PROJECTION_IDENTITY_MISMATCH");
        }

        let mut operation = projection.operation;
        operation.authority_instance_id = self.authority_instance_id.clone();
        Ok(operation)
    }
}

#[tonic::async_trait]
impl WorkspaceApi for WorkspaceControlPlane {
    /// Validates Workspace authority, dispatches a Product-neutral request, and
    /// returns a fail-closed error for malformed or mismatched projections.
    async fn handle_authenticated(
        &self,
        request: WorkspaceApiRequest,
        caller: WorkspaceCallerContext,
    ) -> WorkspaceApiResponse {
        let request_id = request.request_id;
        if request_id.trim().is_empty() {
            return Self::error_response(request_id, 3, "WORKSPACE_REQUEST_ID_REQUIRED");
        }

        if request.workspace_id.trim().is_empty() {
            return Self::error_response(request_id, 3, "WORKSPACE_ID_REQUIRED");
        }
        if request.workspace_id != self.workspace_id {
            return Self::error_response(request_id, 7, "WORKSPACE_AUTHORITY_MISMATCH");
        }

        let Some(request_payload) = request.request else {
            return Self::error_response(request_id, 3, "WORKSPACE_REQUEST_UNSUPPORTED");
        };
        let authorization = match &request_payload {
            workspace_api_request::Request::GetOperation(_) => {
                caller.authorize_workspace_read(&request.workspace_id)
            }
            workspace_api_request::Request::StartOperation(_) => {
                caller.authorize_workspace_command(&request.workspace_id)
            }
            #[allow(unreachable_patterns)]
            _ => caller.authorize_workspace_scope(&request.workspace_id),
        };
        if let Err(error) = authorization {
            return Self::authorization_error_response(request_id, error);
        }
        let (product_request, requested_operation) =
            match Self::validated_product_request(request_payload) {
                Ok(validated) => validated,
                Err(message) => return Self::error_response(request_id, 3, message),
            };

        let projection = match self
            .dispatcher
            .dispatch(&self.workspace_id, product_request)
            .await
        {
            Ok(projection) => projection,
            Err(error) => return Self::dispatch_error_response(request_id, error),
        };
        let operation = match self.validate_projection(projection, &requested_operation) {
            Ok(operation) => operation,
            Err(message) => return Self::error_response(request_id, 13, message),
        };

        WorkspaceApiResponse {
            request_id,
            outcome: Some(workspace_api_response::Outcome::Operation(operation)),
        }
    }
}

impl WorkspaceControlPlane {
    fn authorization_error_response(
        request_id: String,
        error: WorkspaceAuthorizationError,
    ) -> WorkspaceApiResponse {
        let (code, message) = match error {
            WorkspaceAuthorizationError::Unauthenticated => {
                (16, "WORKSPACE_CALLER_CONTEXT_REQUIRED")
            }
            WorkspaceAuthorizationError::WorkspaceMismatch => (7, "WORKSPACE_AUTHORITY_MISMATCH"),
            WorkspaceAuthorizationError::MemberRequired => (7, "WORKSPACE_MEMBERSHIP_DENIED"),
            WorkspaceAuthorizationError::CommandPolicyUnconfigured => {
                (7, "WORKSPACE_COMMAND_POLICY_UNCONFIGURED")
            }
        };
        Self::error_response(request_id, code, message)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use crate::WorkspaceCallerPrincipal;
    use cy_proto::workspace_v1::{
        workspace_api_request, workspace_api_response, UserIdentityRef, WorkspaceOperationState,
    };

    use super::*;

    struct RecordingDispatcher {
        response: Mutex<Option<Result<WorkspaceOperationProjection, WorkspaceDispatchError>>>,
        request: Mutex<Option<WorkspaceProductRequest>>,
    }

    impl RecordingDispatcher {
        fn new(response: Result<WorkspaceOperationProjection, WorkspaceDispatchError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
                request: Mutex::new(None),
            }
        }
    }

    #[tonic::async_trait]
    impl WorkspaceRequestDispatcher for RecordingDispatcher {
        async fn dispatch(
            &self,
            _workspace_id: &str,
            request: WorkspaceProductRequest,
        ) -> Result<WorkspaceOperationProjection, WorkspaceDispatchError> {
            *self.request.lock().expect("request mutex") = Some(request);
            self.response
                .lock()
                .expect("response mutex")
                .take()
                .expect("dispatcher should be called once")
        }
    }

    fn identity(id: &str, generation: u64) -> Identity {
        Identity {
            id: id.to_string(),
            generation,
        }
    }

    fn projection(workspace_id: &str, operation: Identity) -> WorkspaceOperationProjection {
        WorkspaceOperationProjection {
            workspace_id: workspace_id.to_string(),
            operation: WorkspaceOperationView {
                operation: Some(operation),
                state: WorkspaceOperationState::Running as i32,
                completed_units: 2,
                total_units: 8,
                unit: "steps".to_string(),
                artifact_uris: Vec::new(),
                resource_references: Vec::new(),
                status_reason: "running".to_string(),
                authority_instance_id: "dispatcher-value".to_string(),
            },
        }
    }

    fn request(workspace_id: &str, operation: Identity) -> WorkspaceApiRequest {
        WorkspaceApiRequest {
            request_id: "request-1".to_string(),
            workspace_id: workspace_id.to_string(),
            traceparent: String::new(),
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest {
                    operation: Some(operation),
                },
            )),
        }
    }

    fn start_request(
        workspace_id: &str,
        operation: Identity,
        input_artifact_uris: Vec<String>,
    ) -> WorkspaceApiRequest {
        WorkspaceApiRequest {
            request_id: "request-start".to_string(),
            workspace_id: workspace_id.to_string(),
            traceparent: String::new(),
            request: Some(workspace_api_request::Request::StartOperation(
                StartWorkspaceOperationRequest {
                    operation: Some(operation),
                    input_artifact_uris,
                },
            )),
        }
    }

    fn handler(dispatcher: Arc<RecordingDispatcher>) -> WorkspaceControlPlane {
        WorkspaceControlPlane::new("workspace-1", "authority-1", dispatcher)
            .expect("valid authority configuration")
    }

    fn member_caller() -> WorkspaceCallerContext {
        WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-1".to_string(),
            },
            "organization-1",
            "workspace-1",
            BTreeSet::from(["workspace.reader".to_string()]),
        )
        .expect("valid member caller")
    }

    fn error(response: WorkspaceApiResponse) -> RpcStatus {
        match response.outcome {
            Some(workspace_api_response::Outcome::Error(error)) => error,
            _ => panic!("expected a Workspace API error"),
        }
    }

    #[tokio::test]
    async fn dispatches_only_for_its_workspace_and_pins_authority_marker() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher.clone());

        let response = handler
            .handle_authenticated(
                request("workspace-1", identity("operation-1", 4)),
                member_caller(),
            )
            .await;

        let Some(workspace_api_response::Outcome::Operation(operation)) = response.outcome else {
            panic!("expected an operation projection");
        };
        assert_eq!(operation.authority_instance_id, "authority-1");
        assert!(matches!(
            dispatcher.request.lock().expect("request mutex").as_ref(),
            Some(WorkspaceProductRequest::GetOperation(_))
        ));
    }

    #[tokio::test]
    async fn denies_commands_until_a_role_policy_is_configured() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher.clone());

        let status = error(
            handler
                .handle_authenticated(
                    start_request(
                        "workspace-1",
                        identity("operation-1", 4),
                        vec!["artifact://sha256/abc".to_string()],
                    ),
                    member_caller(),
                )
                .await,
        );
        assert_eq!(status.code, 7);
        assert_eq!(status.message, "WORKSPACE_COMMAND_POLICY_UNCONFIGURED");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn rejects_another_workspace_before_dispatch() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher.clone());

        let status = error(
            handler
                .handle_authenticated(
                    request("workspace-2", identity("operation-1", 4)),
                    member_caller(),
                )
                .await,
        );

        assert_eq!(status.code, 7);
        assert_eq!(status.message, "WORKSPACE_AUTHORITY_MISMATCH");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn rejects_a_projection_from_another_workspace() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-2",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher);

        let status = error(
            handler
                .handle_authenticated(
                    request("workspace-1", identity("operation-1", 4)),
                    member_caller(),
                )
                .await,
        );

        assert_eq!(status.code, 13);
        assert_eq!(
            status.message,
            "WORKSPACE_PRODUCT_PROJECTION_AUTHORITY_MISMATCH"
        );
    }

    #[tokio::test]
    async fn rejects_a_projection_for_a_different_operation_identity() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-2", 4),
        ))));
        let handler = handler(dispatcher);

        let status = error(
            handler
                .handle_authenticated(
                    request("workspace-1", identity("operation-1", 4)),
                    member_caller(),
                )
                .await,
        );

        assert_eq!(status.code, 13);
        assert_eq!(
            status.message,
            "WORKSPACE_PRODUCT_PROJECTION_IDENTITY_MISMATCH"
        );
    }

    #[tokio::test]
    async fn maps_dispatch_failures_to_fixed_fail_closed_errors() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Err(
            WorkspaceDispatchError::Unavailable,
        )));
        let handler = handler(dispatcher);

        let status = error(
            handler
                .handle_authenticated(
                    request("workspace-1", identity("operation-1", 4)),
                    member_caller(),
                )
                .await,
        );

        assert_eq!(status.code, 14);
        assert_eq!(status.message, "WORKSPACE_PRODUCT_UNAVAILABLE");
    }

    #[tokio::test]
    async fn legacy_unauthenticated_handler_fails_closed() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher.clone());

        let status = error(
            handler
                .handle(request("workspace-1", identity("operation-1", 4)))
                .await,
        );

        assert_eq!(status.code, 16);
        assert_eq!(status.message, "WORKSPACE_CALLER_CONTEXT_REQUIRED");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn operation_reads_require_directory_member_context() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let handler = handler(dispatcher.clone());
        let nonmember = WorkspaceCallerContext::from_verified(
            WorkspaceCallerPrincipal::User(UserIdentityRef {
                issuer: "https://identity.test".to_string(),
                subject: "user-2".to_string(),
            }),
            "organization-1",
            "workspace-1",
            BTreeSet::new(),
        )
        .expect("valid but nonmember identity");

        let status = error(
            handler
                .handle_authenticated(
                    request("workspace-1", identity("operation-1", 4)),
                    nonmember,
                )
                .await,
        );

        assert_eq!(status.code, 7);
        assert_eq!(status.message, "WORKSPACE_MEMBERSHIP_DENIED");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }
}
