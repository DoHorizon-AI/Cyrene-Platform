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

use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::semantic_v1::Identity;
use cy_proto::workspace_v1::{
    workspace_api_request, workspace_api_response, GetWorkspaceOperationRequest,
    StartWorkspaceOperationRequest, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceOperationView,
};
use cy_workspace_product_contracts::{
    ProductContractBundle, ProductInvocationAdapter, ProductInvocationError, TrustedProductPolicy,
};
use thiserror::Error;

use crate::deployment_admission::{DeploymentAdmission, DeploymentAdmissionError};
use crate::product_authorization::authorize_product_invocation;
use crate::product_projection::{
    product_invocation_rpc_status, validate_product_invocation, validate_product_response,
    MAX_WORKSPACE_ID_BYTES, MAX_WORKSPACE_REQUEST_ID_BYTES,
};
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
    /// A concrete Organization identity is required for authority checks.
    #[error("organization_id must not be empty")]
    EmptyOrganizationId,
    /// A concrete Workspace identity is required for authority checks.
    #[error("workspace_id must not be empty")]
    EmptyWorkspaceId,
    /// An instance marker is required for API projections.
    #[error("authority_instance_id must not be empty")]
    EmptyAuthorityInstanceId,
    /// The configured Control Host admission profile is unknown.
    #[error("control-host admission profile is invalid")]
    InvalidDeploymentAdmissionProfile,
}

/// Application-level Workspace API handler.
///
/// The configured Workspace identity pins authority locally. Product requests
/// are forwarded through `WorkspaceRequestDispatcher`; this type owns no
/// Product records or membership database.
pub struct WorkspaceControlPlane {
    organization_id: String,
    workspace_id: String,
    authority_instance_id: String,
    dispatcher: Arc<dyn WorkspaceRequestDispatcher>,
    product_contracts: Option<Arc<ProductContractBundle>>,
    product_policy: Option<Arc<TrustedProductPolicy>>,
    product_invocation_adapter: Option<Arc<dyn ProductInvocationAdapter>>,
    deployment_admission: DeploymentAdmission,
}

impl WorkspaceControlPlane {
    /// Creates a handler pinned to one Workspace and one authority lifetime.
    ///
    /// # Errors
    /// Returns an error if either identity is empty after trimming whitespace.
    pub fn new(
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        authority_instance_id: impl Into<String>,
        dispatcher: Arc<dyn WorkspaceRequestDispatcher>,
    ) -> Result<Self, WorkspaceControlPlaneConfigError> {
        let organization_id = organization_id.into();
        if organization_id.trim().is_empty() {
            return Err(WorkspaceControlPlaneConfigError::EmptyOrganizationId);
        }

        let workspace_id = workspace_id.into();
        if workspace_id.trim().is_empty() {
            return Err(WorkspaceControlPlaneConfigError::EmptyWorkspaceId);
        }

        let authority_instance_id = authority_instance_id.into();
        if authority_instance_id.trim().is_empty() {
            return Err(WorkspaceControlPlaneConfigError::EmptyAuthorityInstanceId);
        }
        let deployment_admission = DeploymentAdmission::from_environment(
            crate::deployment_admission::DeploymentIdentity {
                organization_id: organization_id.clone(),
                workspace_id: workspace_id.clone(),
                authority_instance_id: authority_instance_id.clone(),
            },
        )
        .map_err(|_| WorkspaceControlPlaneConfigError::InvalidDeploymentAdmissionProfile)?;

        Ok(Self {
            organization_id,
            workspace_id,
            authority_instance_id,
            dispatcher,
            product_contracts: None,
            product_policy: None,
            product_invocation_adapter: None,
            deployment_admission,
        })
    }

    /// Composes v2 catalog, separately pinned policy, and generic Product transport.
    pub fn with_product_v2(
        mut self,
        contracts: Arc<ProductContractBundle>,
        policy: Arc<TrustedProductPolicy>,
        adapter: Arc<dyn ProductInvocationAdapter>,
    ) -> Self {
        self.product_contracts = Some(contracts);
        self.product_policy = Some(policy);
        self.product_invocation_adapter = Some(adapter);
        self
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
        if request_id.len() > MAX_WORKSPACE_REQUEST_ID_BYTES {
            return Self::error_response(request_id, 3, "WORKSPACE_REQUEST_ID_INVALID");
        }

        if request.workspace_id.trim().is_empty() {
            return Self::error_response(request_id, 3, "WORKSPACE_ID_REQUIRED");
        }
        if request.workspace_id.len() > MAX_WORKSPACE_ID_BYTES {
            return Self::error_response(request_id, 3, "WORKSPACE_ID_INVALID");
        }
        if request.workspace_id != self.workspace_id {
            return Self::error_response(request_id, 7, "WORKSPACE_AUTHORITY_MISMATCH");
        }
        if caller.organization_id() != self.organization_id {
            return Self::error_response(request_id, 7, "ORGANIZATION_AUTHORITY_MISMATCH");
        }

        let Some(request_payload) = request.request else {
            return Self::error_response(request_id, 3, "WORKSPACE_REQUEST_UNSUPPORTED");
        };
        match &request_payload {
            workspace_api_request::Request::ProductApiV2(invocation) => {
                return self
                    .invoke_product_api_v2(request_id, invocation.clone(), caller)
                    .await;
            }
            workspace_api_request::Request::ProductApi(_) => {
                return Self::error_response(request_id, 3, "WORKSPACE_PRODUCT_API_V1_UNSUPPORTED");
            }
            _ => {}
        }

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

        let _admission = match self
            .deployment_admission
            .acquire_dispatch(
                &self.organization_id,
                &self.workspace_id,
                &self.authority_instance_id,
            )
            .await
        {
            Ok(guard) => guard,
            Err(DeploymentAdmissionError::Closed) => {
                return Self::dispatch_error_response(
                    request_id,
                    WorkspaceDispatchError::FailedPrecondition,
                )
            }
            Err(DeploymentAdmissionError::Unavailable) => {
                return Self::dispatch_error_response(
                    request_id,
                    WorkspaceDispatchError::Unavailable,
                )
            }
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
    async fn invoke_product_api_v2(
        &self,
        request_id: String,
        invocation: ProductApiInvocationV2,
        caller: WorkspaceCallerContext,
    ) -> WorkspaceApiResponse {
        let invocation = match validate_product_invocation(invocation) {
            Ok(invocation) => invocation,
            Err(error) => return Self::product_invocation_error_response(request_id, error),
        };
        let (Some(contracts), Some(policy), Some(adapter)) = (
            self.product_contracts.as_ref(),
            self.product_policy.as_ref(),
            self.product_invocation_adapter.as_ref(),
        ) else {
            return Self::product_invocation_error_response(
                request_id,
                ProductInvocationError::Unavailable,
            );
        };
        let authorized = match authorize_product_invocation(
            contracts,
            policy,
            invocation,
            &caller,
            &self.organization_id,
            &self.workspace_id,
        ) {
            Ok(authorized) => authorized,
            Err(error) => return Self::product_invocation_error_response(request_id, error),
        };
        let _admission = match self
            .deployment_admission
            .acquire_dispatch(
                &self.organization_id,
                &self.workspace_id,
                &self.authority_instance_id,
            )
            .await
        {
            Ok(guard) => guard,
            Err(DeploymentAdmissionError::Closed) => {
                return Self::product_invocation_error_response(
                    request_id,
                    ProductInvocationError::FailedPrecondition,
                )
            }
            Err(DeploymentAdmissionError::Unavailable) => {
                return Self::product_invocation_error_response(
                    request_id,
                    ProductInvocationError::Unavailable,
                )
            }
        };
        let response = match adapter.invoke(&authorized).await {
            Ok(response) => response,
            Err(error) => return Self::product_invocation_error_response(request_id, error),
        };
        if let Err(error) = authorized.validate_response(&response) {
            return Self::product_invocation_error_response(request_id, error);
        }
        let response = match validate_product_response(response) {
            Ok(response) => response,
            Err(error) => return Self::product_invocation_error_response(request_id, error),
        };
        WorkspaceApiResponse {
            request_id,
            outcome: Some(workspace_api_response::Outcome::ProductApiV2(response)),
        }
    }

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

    fn product_invocation_error_response(
        request_id: String,
        error: ProductInvocationError,
    ) -> WorkspaceApiResponse {
        let (code, message) = product_invocation_rpc_status(error);
        Self::error_response(request_id, code, message)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
    use cy_proto::workspace_v1::{
        workspace_api_request, workspace_api_response, UserIdentityRef, WorkspaceOperationState,
        WorkspaceProductApiRequest,
    };

    use super::*;
    #[cfg(unix)]
    use crate::deployment_admission::CONTROL_HOST_ADMISSION_SCHEMA;
    use crate::PRODUCT_JSON_BODY_MAX_BYTES;
    #[cfg(unix)]
    use tempfile::tempdir;

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
            id: id.to_owned(),
            generation,
        }
    }

    fn projection(workspace_id: &str, operation: Identity) -> WorkspaceOperationProjection {
        WorkspaceOperationProjection {
            workspace_id: workspace_id.to_owned(),
            operation: WorkspaceOperationView {
                operation: Some(operation),
                state: WorkspaceOperationState::Running as i32,
                completed_units: 2,
                total_units: 8,
                unit: "steps".to_owned(),
                artifact_uris: Vec::new(),
                resource_references: Vec::new(),
                status_reason: "running".to_owned(),
                authority_instance_id: "dispatcher-value".to_owned(),
            },
        }
    }

    fn request(workspace_id: &str, operation: Identity) -> WorkspaceApiRequest {
        WorkspaceApiRequest {
            request_id: "request-1".to_owned(),
            workspace_id: workspace_id.to_owned(),
            traceparent: String::new(),
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest {
                    operation: Some(operation),
                },
            )),
        }
    }

    fn handler(dispatcher: Arc<RecordingDispatcher>) -> WorkspaceControlPlane {
        WorkspaceControlPlane::new("organization-1", "workspace-1", "authority-1", dispatcher)
            .expect("valid authority configuration")
    }

    fn member_caller() -> WorkspaceCallerContext {
        WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_owned(),
                subject: "user-1".to_owned(),
            },
            "organization-1",
            "workspace-1",
            BTreeSet::from(["workspace.reader".to_owned()]),
        )
        .expect("valid member caller")
    }

    fn error(response: WorkspaceApiResponse) -> RpcStatus {
        match response.outcome {
            Some(workspace_api_response::Outcome::Error(error)) => error,
            _ => panic!("expected a Workspace API error"),
        }
    }

    fn product_request(request: workspace_api_request::Request) -> WorkspaceApiRequest {
        WorkspaceApiRequest {
            request_id: "request-product".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            traceparent: String::new(),
            request: Some(request),
        }
    }

    #[tokio::test]
    async fn dispatches_only_for_its_workspace_and_pins_authority_marker() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let response = handler(dispatcher.clone())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn closed_control_host_admission_blocks_product_dispatch_under_the_live_gate() {
        let directory = tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(directory.path().join("admission.lock"), b"").unwrap();
        fs::set_permissions(
            directory.path().join("admission.lock"),
            fs::Permissions::from_mode(0o660),
        )
        .unwrap();
        let receipt = serde_json::json!({
            "schema": CONTROL_HOST_ADMISSION_SCHEMA,
            "source": "control-host-adoption-helper.v1",
            "scope": "workspace-product-v2/control-host",
            "state": "closed",
            "generation": 1,
            "readerGid": nix::unistd::getegid().as_raw(),
            "catalogDigest": format!("sha256:{}", "a".repeat(64)),
            "topologyDigest": format!("sha256:{}", "b".repeat(64)),
            "planDigest": format!("sha256:{}", "c".repeat(64)),
            "adoptionHold": {
                "requestId": "adoption-1",
                "organizationId": "organization-1",
                "workspaceId": "workspace-1",
                "authorityInstanceId": "authority-1",
                "phase": "ACTIVE_HELD",
                "planDigest": format!("sha256:{}", "c".repeat(64))
            }
        });
        fs::write(
            directory.path().join("state.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            directory.path().join("state.json"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();

        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let mut control_plane = handler(dispatcher.clone());
        control_plane.deployment_admission = DeploymentAdmission::for_test(
            directory.path().to_path_buf(),
            crate::deployment_admission::DeploymentIdentity {
                organization_id: "organization-1".to_owned(),
                workspace_id: "workspace-1".to_owned(),
                authority_instance_id: "authority-1".to_owned(),
            },
        );

        let status = error(
            control_plane
                .handle_authenticated(
                    request("workspace-1", identity("operation-1", 4)),
                    member_caller(),
                )
                .await,
        );
        assert_eq!(status.code, 9);
        assert_eq!(status.message, "WORKSPACE_REQUEST_FAILED_PRECONDITION");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn rejects_caller_organization_mismatch_before_dispatch() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Ok(projection(
            "workspace-1",
            identity("operation-1", 4),
        ))));
        let caller = WorkspaceCallerContext::user_member(
            UserIdentityRef {
                issuer: "https://identity.test".to_owned(),
                subject: "user-1".to_owned(),
            },
            "organization-2",
            "workspace-1",
            BTreeSet::new(),
        )
        .expect("valid member caller");

        let status = error(
            handler(dispatcher.clone())
                .handle_authenticated(request("workspace-1", identity("operation-1", 4)), caller)
                .await,
        );
        assert_eq!(status.code, 7);
        assert_eq!(status.message, "ORGANIZATION_AUTHORITY_MISMATCH");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn legacy_product_api_is_explicitly_rejected_without_v1_fallback() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Err(
            WorkspaceDispatchError::Unavailable,
        )));
        let response = product_request(workspace_api_request::Request::ProductApi(
            WorkspaceProductApiRequest::default(),
        ));
        let status = error(
            handler(dispatcher.clone())
                .handle_authenticated(response, member_caller())
                .await,
        );
        assert_eq!(status.code, 3);
        assert_eq!(status.message, "WORKSPACE_PRODUCT_API_V1_UNSUPPORTED");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn v2_product_call_fails_closed_until_release_policy_and_adapter_are_composed() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Err(
            WorkspaceDispatchError::Unavailable,
        )));
        let response = product_request(workspace_api_request::Request::ProductApiV2(
            ProductApiInvocationV2 {
                owner_id: "catalyst".to_owned(),
                operation_id: "workspaceListDatasets".to_owned(),
                ..Default::default()
            },
        ));
        let status = error(
            handler(dispatcher.clone())
                .handle_authenticated(response, member_caller())
                .await,
        );
        assert_eq!(status.code, 14);
        assert_eq!(status.message, "WORKSPACE_PRODUCT_API_UNAVAILABLE");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }

    #[tokio::test]
    async fn oversized_v2_payload_is_rejected_before_policy_or_network_dispatch() {
        let dispatcher = Arc::new(RecordingDispatcher::new(Err(
            WorkspaceDispatchError::Unavailable,
        )));
        let response = product_request(workspace_api_request::Request::ProductApiV2(
            ProductApiInvocationV2 {
                owner_id: "catalyst".to_owned(),
                operation_id: "workspaceListDatasets".to_owned(),
                json_body: vec![b'x'; PRODUCT_JSON_BODY_MAX_BYTES + 1],
                ..Default::default()
            },
        ));
        let status = error(
            handler(dispatcher.clone())
                .handle_authenticated(response, member_caller())
                .await,
        );
        assert_eq!(status.code, 3);
        assert_eq!(status.message, "WORKSPACE_PRODUCT_API_REQUEST_INVALID");
        assert!(dispatcher.request.lock().expect("request mutex").is_none());
    }
}
