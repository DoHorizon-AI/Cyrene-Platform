//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 fabric_gateway.rs                                               │
//! │  Module: cy_workspace_web_bff::fabric_gateway                      │
//! │  Role: Bind member requests to Directory-selected Workspace APIs.  │
//! │                                                                     │
//! │  模块职责：将成员请求绑定到 Directory 选出的 Workspace API。            │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cy_workspace_fabric::workspace_v1::{
    workspace_api_request, WorkspaceApiRequest, WorkspaceApiResponse, WorkspaceConnectionCandidate,
    WorkspaceConnectionDescriptor,
};
use cy_workspace_fabric::{
    validate_descriptor, VerifiedWebPrincipal, WorkspaceApi, WorkspaceCallerContext,
    WorkspaceDirectory, WorkspaceDirectoryError,
};

use crate::WorkspaceGatewayError;

/// Resolve one Directory-owned Workspace descriptor to its authenticated API transport.
///
/// Implementations must bind the returned handle to a candidate from the exact
/// descriptor supplied here. No request URL, authority, or Workspace identifier
/// is accepted as resolver input.
///
/// 将 Directory 所有的 Workspace descriptor 解析到已认证 API transport。
/// 实现必须将返回句柄绑定到本次 descriptor 中的 candidate；resolver 不接收 request URL、authority 或其他 Workspace ID。
#[async_trait]
pub trait WorkspaceApiResolver: Send + Sync + 'static {
    /// Resolve an API handle for a trusted, currently valid descriptor.
    async fn resolve(
        &self,
        descriptor: &WorkspaceConnectionDescriptor,
    ) -> Result<WorkspaceApiBinding, WorkspaceApiResolutionError>;
}

/// Workspace API handle bound to one descriptor and one of its candidates.
///
/// A resolver constructs this only after establishing that its transport is
/// authenticated as the selected Workspace authority.
pub struct WorkspaceApiBinding {
    descriptor_version: String,
    workspace_id: String,
    organization_id: String,
    candidate: WorkspaceConnectionCandidate,
    api: Arc<dyn WorkspaceApi>,
}

impl WorkspaceApiBinding {
    /// Bind an authenticated API transport to an exact Directory candidate.
    pub fn for_candidate(
        descriptor: &WorkspaceConnectionDescriptor,
        candidate: &WorkspaceConnectionCandidate,
        api: Arc<dyn WorkspaceApi>,
    ) -> Result<Self, WorkspaceApiResolutionError> {
        if descriptor.workspace_id.trim().is_empty()
            || descriptor.organization_id.trim().is_empty()
            || !descriptor.candidates.contains(candidate)
        {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }
        Ok(Self {
            descriptor_version: descriptor.descriptor_version.clone(),
            workspace_id: descriptor.workspace_id.clone(),
            organization_id: descriptor.organization_id.clone(),
            candidate: candidate.clone(),
            api,
        })
    }

    fn belongs_to(&self, descriptor: &WorkspaceConnectionDescriptor) -> bool {
        self.descriptor_version == descriptor.descriptor_version
            && self.workspace_id == descriptor.workspace_id
            && self.organization_id == descriptor.organization_id
            && descriptor.candidates.contains(&self.candidate)
    }
}

impl std::fmt::Debug for WorkspaceApiBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceApiBinding")
            .field("descriptor_version", &self.descriptor_version)
            .field("workspace_id", &self.workspace_id)
            .field("organization_id", &self.organization_id)
            .field("candidate_configured", &true)
            .field("api_configured", &true)
            .finish()
    }
}

/// Safe resolver failures; implementation diagnostics and endpoint values stay private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceApiResolutionError {
    /// No trusted endpoint-to-authority mapping is configured for this descriptor.
    #[error("WORKSPACE_API_RESOLVER_NOT_CONFIGURED")]
    NotConfigured,
    /// The configured transport cannot currently reach the selected authority.
    #[error("WORKSPACE_API_RESOLVER_UNAVAILABLE")]
    Unavailable,
    /// The resolver returned a binding outside the supplied descriptor.
    #[error("WORKSPACE_API_RESOLVER_BINDING_INVALID")]
    InvalidBinding,
}

/// BFF gateway that derives scope and roles from the same authoritative Directory.
pub struct FabricWorkspaceProductGateway {
    directory: Arc<dyn WorkspaceDirectory>,
    resolver: Arc<dyn WorkspaceApiResolver>,
}

impl FabricWorkspaceProductGateway {
    /// Create a gateway with required Workspace membership and authority resolvers.
    pub fn new(
        directory: Arc<dyn WorkspaceDirectory>,
        resolver: Arc<dyn WorkspaceApiResolver>,
    ) -> Self {
        Self {
            directory,
            resolver,
        }
    }
}

#[async_trait]
impl crate::WorkspaceProductGateway for FabricWorkspaceProductGateway {
    async fn invoke(
        &self,
        principal: &VerifiedWebPrincipal,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, WorkspaceGatewayError> {
        validate_gateway_request(&request)?;

        let now = now_unix_ms().ok_or(WorkspaceGatewayError::Unavailable)?;
        if principal.expires_at_unix_ms() <= now {
            return Err(WorkspaceGatewayError::Forbidden);
        }
        let descriptor = select_member_workspace(
            self.directory.as_ref(),
            principal.identity(),
            principal.organization_id(),
            &request.workspace_id,
            u64::try_from(now).map_err(|_| WorkspaceGatewayError::Unavailable)?,
        )
        .await?;
        let _preflight_caller = WorkspaceCallerContext::from_verified_web_member(
            principal,
            descriptor.workspace_id.clone(),
            self.directory.as_ref(),
        )
        .await
        .map_err(|error| map_caller_context_status(error.http_status()))?;
        let binding = self
            .resolver
            .resolve(&descriptor)
            .await
            .map_err(map_resolution_error)?;
        if !binding.belongs_to(&descriptor) {
            return Err(WorkspaceGatewayError::InvalidResponse);
        }

        let dispatch_now = now_unix_ms().ok_or(WorkspaceGatewayError::Unavailable)?;
        if principal.expires_at_unix_ms() <= dispatch_now {
            return Err(WorkspaceGatewayError::Forbidden);
        }
        validate_descriptor(
            &descriptor,
            u64::try_from(dispatch_now).map_err(|_| WorkspaceGatewayError::Unavailable)?,
        )
        .map_err(|_| WorkspaceGatewayError::InvalidResponse)?;
        let caller = WorkspaceCallerContext::from_verified_web_member(
            principal,
            descriptor.workspace_id.clone(),
            self.directory.as_ref(),
        )
        .await
        .map_err(|error| map_caller_context_status(error.http_status()))?;
        if caller.organization_id() != descriptor.organization_id
            || caller.workspace_id() != descriptor.workspace_id
            || request.workspace_id != descriptor.workspace_id
        {
            return Err(WorkspaceGatewayError::Forbidden);
        }

        let expected_request_id = request.request_id.clone();
        let response = binding.api.handle_authenticated(request, caller).await;
        if response.request_id != expected_request_id {
            return Err(WorkspaceGatewayError::InvalidResponse);
        }
        Ok(response)
    }
}

async fn select_member_workspace(
    directory: &dyn WorkspaceDirectory,
    identity: &cy_workspace_fabric::workspace_v1::UserIdentityRef,
    organization_id: &str,
    workspace_id: &str,
    now_unix_ms: u64,
) -> Result<WorkspaceConnectionDescriptor, WorkspaceGatewayError> {
    let descriptors = directory
        .discover(identity, organization_id, now_unix_ms)
        .await
        .map_err(map_directory_error)?;
    let mut seen = BTreeSet::new();
    let mut selected = None;
    for descriptor in descriptors {
        validate_descriptor(&descriptor, now_unix_ms)
            .map_err(|_| WorkspaceGatewayError::InvalidResponse)?;
        if descriptor.organization_id != organization_id
            || !seen.insert(descriptor.workspace_id.clone())
        {
            return Err(WorkspaceGatewayError::InvalidResponse);
        }
        if descriptor.workspace_id == workspace_id {
            selected = Some(descriptor);
        }
    }
    selected.ok_or(WorkspaceGatewayError::NotFound)
}

fn validate_gateway_request(request: &WorkspaceApiRequest) -> Result<(), WorkspaceGatewayError> {
    if request.request_id.trim().is_empty()
        || request.request_id.len() > 256
        || request.workspace_id.trim().is_empty()
        || request.workspace_id.len() > 200
        || !matches!(
            request.request.as_ref(),
            Some(workspace_api_request::Request::ProductApi(_))
        )
    {
        return Err(WorkspaceGatewayError::InvalidResponse);
    }
    Ok(())
}

fn map_caller_context_status(status: u16) -> WorkspaceGatewayError {
    match status {
        404 => WorkspaceGatewayError::NotFound,
        503 => WorkspaceGatewayError::Unavailable,
        401 | 403 => WorkspaceGatewayError::Forbidden,
        _ => WorkspaceGatewayError::InvalidResponse,
    }
}

fn map_directory_error(error: WorkspaceDirectoryError) -> WorkspaceGatewayError {
    match error {
        WorkspaceDirectoryError::Identity(_) => WorkspaceGatewayError::Forbidden,
        WorkspaceDirectoryError::Descriptor(_) => WorkspaceGatewayError::InvalidResponse,
        WorkspaceDirectoryError::Storage(_) => WorkspaceGatewayError::Unavailable,
    }
}

fn map_resolution_error(error: WorkspaceApiResolutionError) -> WorkspaceGatewayError {
    match error {
        WorkspaceApiResolutionError::NotConfigured | WorkspaceApiResolutionError::Unavailable => {
            WorkspaceGatewayError::Unavailable
        }
        WorkspaceApiResolutionError::InvalidBinding => WorkspaceGatewayError::InvalidResponse,
    }
}

fn now_unix_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use cy_workspace_fabric::workspace_v1::{
        workspace_api_request, GetWorkspaceOperationRequest, WorkspaceApiRequest,
        WorkspaceConnectionCandidate, WorkspaceConnectionDescriptor,
    };
    use cy_workspace_fabric::{WorkspaceDirectory, WorkspaceDirectoryError};
    use prost_types::Timestamp;

    use super::*;

    struct FixtureDirectory {
        descriptors: Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError>,
        calls: Mutex<Vec<(String, String, u64)>>,
    }

    #[async_trait]
    impl WorkspaceDirectory for FixtureDirectory {
        async fn discover(
            &self,
            user: &cy_workspace_fabric::workspace_v1::UserIdentityRef,
            organization_id: &str,
            now_unix_ms: u64,
        ) -> Result<Vec<WorkspaceConnectionDescriptor>, WorkspaceDirectoryError> {
            self.calls.lock().expect("calls mutex").push((
                user.subject.clone(),
                organization_id.to_string(),
                now_unix_ms,
            ));
            self.descriptors.clone()
        }

        async fn is_member(
            &self,
            _user: &cy_workspace_fabric::workspace_v1::UserIdentityRef,
            _organization_id: &str,
            _workspace_id: &str,
        ) -> Result<bool, WorkspaceDirectoryError> {
            Ok(true)
        }
    }

    fn descriptor(workspace_id: &str, organization_id: &str) -> WorkspaceConnectionDescriptor {
        WorkspaceConnectionDescriptor {
            descriptor_version: "cyrene.workspace.connection.v1".to_string(),
            workspace_id: workspace_id.to_string(),
            organization_id: organization_id.to_string(),
            display_name: format!("Workspace {workspace_id}"),
            candidates: vec![WorkspaceConnectionCandidate {
                mode: cy_proto::core_v1::ConnectivityMode::Relay as i32,
                provider_id: "relay-provider".to_string(),
                connection_uri: "https://relay.internal.example/workspace".to_string(),
                server_name: "relay.internal.example".to_string(),
                priority: 1,
                routing_hint: Vec::new(),
            }],
            expires_at: Some(Timestamp {
                seconds: 4_102_444_800,
                nanos: 0,
            }),
        }
    }

    fn identity() -> cy_workspace_fabric::workspace_v1::UserIdentityRef {
        cy_workspace_fabric::workspace_v1::UserIdentityRef {
            issuer: "https://issuer.example".to_string(),
            subject: "subject-1".to_string(),
        }
    }

    #[tokio::test]
    async fn exact_workspace_is_selected_from_multiple_directory_results() {
        let directory = FixtureDirectory {
            descriptors: Ok(vec![
                descriptor("workspace-a", "org-1"),
                descriptor("workspace-b", "org-1"),
            ]),
            calls: Mutex::new(Vec::new()),
        };
        let selected = select_member_workspace(
            &directory,
            &identity(),
            "org-1",
            "workspace-b",
            1_800_000_000_000,
        )
        .await
        .expect("exact member workspace should be selected");

        assert_eq!(selected.workspace_id, "workspace-b");
        assert_eq!(directory.calls.lock().expect("calls mutex").len(), 1);
    }

    #[tokio::test]
    async fn cross_organization_directory_descriptor_is_rejected() {
        let directory = FixtureDirectory {
            descriptors: Ok(vec![descriptor("workspace-a", "org-other")]),
            calls: Mutex::new(Vec::new()),
        };

        assert_eq!(
            select_member_workspace(
                &directory,
                &identity(),
                "org-1",
                "workspace-a",
                1_800_000_000_000,
            )
            .await
            .expect_err("cross-organization data must fail closed"),
            WorkspaceGatewayError::InvalidResponse
        );
    }

    #[tokio::test]
    async fn missing_descriptor_and_duplicate_workspace_selection_are_rejected() {
        let missing = FixtureDirectory {
            descriptors: Ok(vec![descriptor("workspace-a", "org-1")]),
            calls: Mutex::new(Vec::new()),
        };
        assert_eq!(
            select_member_workspace(
                &missing,
                &identity(),
                "org-1",
                "workspace-missing",
                1_800_000_000_000,
            )
            .await
            .expect_err("unknown workspace must not select an endpoint"),
            WorkspaceGatewayError::NotFound
        );

        let duplicate = FixtureDirectory {
            descriptors: Ok(vec![
                descriptor("workspace-a", "org-1"),
                descriptor("workspace-a", "org-1"),
            ]),
            calls: Mutex::new(Vec::new()),
        };
        assert_eq!(
            select_member_workspace(
                &duplicate,
                &identity(),
                "org-1",
                "workspace-a",
                1_800_000_000_000,
            )
            .await
            .expect_err("ambiguous workspace selection must fail closed"),
            WorkspaceGatewayError::InvalidResponse
        );
    }

    #[tokio::test]
    async fn directory_outage_is_unavailable() {
        let directory = FixtureDirectory {
            descriptors: Err(WorkspaceDirectoryError::Storage(
                "private detail".to_string(),
            )),
            calls: Mutex::new(Vec::new()),
        };

        assert_eq!(
            select_member_workspace(
                &directory,
                &identity(),
                "org-1",
                "workspace-a",
                1_800_000_000_000,
            )
            .await
            .expect_err("Directory outage must fail closed"),
            WorkspaceGatewayError::Unavailable
        );
    }

    #[test]
    fn resolver_binding_must_use_a_candidate_from_the_selected_descriptor() {
        let first = descriptor("workspace-a", "org-1");
        let second = descriptor("workspace-b", "org-1");
        let api: Arc<dyn WorkspaceApi> = Arc::new(EmptyApi);
        let binding =
            WorkspaceApiBinding::for_candidate(&first, &first.candidates[0], Arc::clone(&api))
                .expect("trusted candidate can bind an API");

        assert!(binding.belongs_to(&first));
        assert!(!binding.belongs_to(&second));
        let mut unlisted_candidate = second.candidates[0].clone();
        unlisted_candidate.provider_id = "unlisted-provider".to_string();
        assert_eq!(
            WorkspaceApiBinding::for_candidate(&first, &unlisted_candidate, api)
                .expect_err("candidate absent from the selected descriptor must be rejected"),
            WorkspaceApiResolutionError::InvalidBinding
        );
    }

    #[test]
    fn gateway_accepts_only_typed_product_requests_without_caller_or_url_fields() {
        let valid = WorkspaceApiRequest {
            request_id: "request-1".to_string(),
            workspace_id: "workspace-a".to_string(),
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            request: Some(workspace_api_request::Request::ProductApi(
                cy_workspace_fabric::workspace_v1::WorkspaceProductApiRequest::default(),
            )),
        };
        assert!(validate_gateway_request(&valid).is_ok());

        let non_product = WorkspaceApiRequest {
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest::default(),
            )),
            ..valid
        };
        assert_eq!(
            validate_gateway_request(&non_product),
            Err(WorkspaceGatewayError::InvalidResponse)
        );
    }

    struct EmptyApi;

    #[async_trait]
    impl WorkspaceApi for EmptyApi {}
}
