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
use cy_proto::cyrene::workspace::authority::v2 as authority_v2;
use cy_proto::cyrene::workspace::authority::v2::workspace_authority_service_client::WorkspaceAuthorityServiceClient;
use cy_workspace_control_plane::workspace_v1::{
    workspace_api_request, WorkspaceApiRequest, WorkspaceApiResponse, WorkspaceConnectionCandidate,
    WorkspaceConnectionDescriptor,
};
use cy_workspace_control_plane::{
    validate_descriptor, VerifiedWebPrincipal, WorkspaceApi, WorkspaceCallerContext,
    WorkspaceDirectory, WorkspaceDirectoryError,
};
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use tonic::Request as TonicRequest;

use crate::WorkspaceGatewayError;

/// BFF gateway which obtains catalog truth and invocation results only from the separate local
/// Platform Authority process. It never calls Plugins Bridge or the outbox directly.
pub struct AuthorityWorkspaceProductGateway {
    directory: Arc<dyn WorkspaceDirectory>,
    authority_uds: std::path::PathBuf,
}

impl AuthorityWorkspaceProductGateway {
    /// Create a gateway bound to the fixed local Authority UDS.
    pub fn new(
        directory: Arc<dyn WorkspaceDirectory>,
        authority_uds: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            directory,
            authority_uds: authority_uds.into(),
        }
    }

    async fn connect(
        &self,
    ) -> Result<WorkspaceAuthorityServiceClient<Channel>, WorkspaceGatewayError> {
        #[cfg(unix)]
        {
            let path = self.authority_uds.clone();
            let channel = Endpoint::try_from("http://[::]:50051")
                .map_err(|_| WorkspaceGatewayError::Unavailable)?
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::tokio::TokioIo::new(stream))
                    }
                }))
                .await
                .map_err(|_| WorkspaceGatewayError::Unavailable)?;
            Ok(WorkspaceAuthorityServiceClient::new(channel))
        }
        #[cfg(not(unix))]
        {
            let _ = &self.authority_uds;
            Err(WorkspaceGatewayError::Unavailable)
        }
    }
}

#[async_trait]
impl crate::WorkspaceProductGateway for AuthorityWorkspaceProductGateway {
    async fn catalog_for_request(
        &self,
        principal: &VerifiedWebPrincipal,
        access_token: &str,
        workspace_id: &str,
    ) -> Result<crate::ProductOperationCatalog, WorkspaceGatewayError> {
        let now = now_unix_ms().ok_or(WorkspaceGatewayError::Unavailable)?;
        if principal.expires_at_unix_ms() <= now {
            return Err(WorkspaceGatewayError::Forbidden);
        }
        WorkspaceCallerContext::from_verified_web_member(
            principal,
            workspace_id,
            self.directory.as_ref(),
        )
        .await
        .map_err(|_| WorkspaceGatewayError::Forbidden)?;
        let mut client = self.connect().await?;
        let mut request = TonicRequest::new(authority_v2::CatalogSnapshotRequest {
            workspace_id: workspace_id.to_owned(),
        });
        request
            .metadata_mut()
            .insert("authorization", bearer_metadata(access_token)?);
        let view = client
            .get_catalog_snapshot(request)
            .await
            .map_err(map_authority_status)?
            .into_inner();
        if view.organization_id != principal.organization_id() || view.workspace_id != workspace_id
        {
            return Err(WorkspaceGatewayError::InvalidResponse);
        }
        crate::load_product_operation_catalog_from_environment(&view)
            .map_err(|_| WorkspaceGatewayError::Unavailable)
    }

    async fn invoke(
        &self,
        principal: &VerifiedWebPrincipal,
        access_token: &str,
        request: WorkspaceApiRequest,
    ) -> Result<WorkspaceApiResponse, WorkspaceGatewayError> {
        validate_gateway_request(&request)?;
        let now = now_unix_ms().ok_or(WorkspaceGatewayError::Unavailable)?;
        if principal.expires_at_unix_ms() <= now {
            return Err(WorkspaceGatewayError::Forbidden);
        }
        WorkspaceCallerContext::from_verified_web_member(
            principal,
            request.workspace_id.clone(),
            self.directory.as_ref(),
        )
        .await
        .map_err(|_| WorkspaceGatewayError::Forbidden)?;
        let Some(workspace_api_request::Request::ProductApiV2(invocation)) = request.request else {
            return Err(WorkspaceGatewayError::InvalidResponse);
        };
        let mut client = self.connect().await?;
        let mut approve = TonicRequest::new(authority_v2::ApproveAndEnqueueInvocationRequest {
            workspace_id: request.workspace_id.clone(),
            invocation: Some(invocation),
        });
        approve
            .metadata_mut()
            .insert("authorization", bearer_metadata(access_token)?);
        let approved = client
            .approve_and_enqueue_invocation(approve)
            .await
            .map_err(map_authority_status)?
            .into_inner();
        if approved.invocation_id.is_empty()
            || !matches!(
                authority_v2::InvocationState::try_from(approved.state),
                Ok(authority_v2::InvocationState::Pending
                    | authority_v2::InvocationState::Claimed
                    | authority_v2::InvocationState::Acknowledged
                    | authority_v2::InvocationState::Succeeded)
            )
        {
            return Err(WorkspaceGatewayError::InvalidResponse);
        }
        let mut wait = TonicRequest::new(authority_v2::WaitInvocationResultRequest {
            invocation_id: approved.invocation_id,
            timeout_ms: 30_000,
            workspace_id: request.workspace_id.clone(),
        });
        wait.metadata_mut()
            .insert("authorization", bearer_metadata(access_token)?);
        let result = client
            .wait_invocation_result(wait)
            .await
            .map_err(map_authority_status)?
            .into_inner();
        if authority_v2::InvocationState::try_from(result.state).ok()
            != Some(authority_v2::InvocationState::Succeeded)
            || result.product_response.is_none()
            || result.result_receipt.is_empty()
        {
            return Err(
                match authority_v2::InvocationState::try_from(result.state).ok() {
                    Some(authority_v2::InvocationState::Failed) => {
                        WorkspaceGatewayError::InvalidResponse
                    }
                    Some(
                        authority_v2::InvocationState::Pending
                        | authority_v2::InvocationState::Claimed
                        | authority_v2::InvocationState::Acknowledged,
                    ) => WorkspaceGatewayError::Timeout,
                    _ => WorkspaceGatewayError::Unavailable,
                },
            );
        }
        Ok(WorkspaceApiResponse {
            request_id: request.request_id,
            outcome: Some(cy_workspace_control_plane::workspace_v1::workspace_api_response::Outcome::ProductApiV2(
                result.product_response.expect("checked present"),
            )),
        })
    }
}

fn bearer_metadata(
    token: &str,
) -> Result<MetadataValue<tonic::metadata::Ascii>, WorkspaceGatewayError> {
    if token.is_empty() || token.len() > 16 * 1024 || token.trim() != token {
        return Err(WorkspaceGatewayError::Forbidden);
    }
    MetadataValue::try_from(format!("Bearer {token}")).map_err(|_| WorkspaceGatewayError::Forbidden)
}

fn map_authority_status(status: tonic::Status) -> WorkspaceGatewayError {
    match status.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => {
            WorkspaceGatewayError::Forbidden
        }
        tonic::Code::NotFound => WorkspaceGatewayError::NotFound,
        tonic::Code::DeadlineExceeded => WorkspaceGatewayError::Timeout,
        tonic::Code::InvalidArgument | tonic::Code::FailedPrecondition | tonic::Code::Aborted => {
            WorkspaceGatewayError::InvalidResponse
        }
        _ => WorkspaceGatewayError::Unavailable,
    }
}

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
    /// Resolve a fresh API handle for this verified user and valid descriptor.
    ///
    /// Implementations must bind the returned transport to this principal. Do not cache or
    /// reuse a Frontend Relay session across distinct principals, even when they select the
    /// same Workspace descriptor.
    ///
    /// 为当前已验证用户和 descriptor 创建专属 API handle。不得在不同 principal 间缓存或复用 Frontend Relay session。
    async fn resolve(
        &self,
        principal: &VerifiedWebPrincipal,
        access_token: &str,
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
    principal: VerifiedPrincipalBinding,
    api: Arc<dyn WorkspaceApi>,
}

impl WorkspaceApiBinding {
    /// Bind a principal-scoped API transport to an exact Directory candidate.
    ///
    /// 将 principal 作用域内的 API transport 绑定到 Directory 中精确的 candidate。
    pub fn for_candidate(
        principal: &VerifiedWebPrincipal,
        descriptor: &WorkspaceConnectionDescriptor,
        candidate: &WorkspaceConnectionCandidate,
        api: Arc<dyn WorkspaceApi>,
    ) -> Result<Self, WorkspaceApiResolutionError> {
        if descriptor.workspace_id.trim().is_empty()
            || descriptor.organization_id.trim().is_empty()
            || descriptor.organization_id != principal.organization_id()
            || !descriptor_has_candidate(descriptor, candidate)
        {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }
        Ok(Self {
            descriptor_version: descriptor.descriptor_version.clone(),
            workspace_id: descriptor.workspace_id.clone(),
            organization_id: descriptor.organization_id.clone(),
            candidate: candidate.clone(),
            principal: VerifiedPrincipalBinding::from_principal(principal)?,
            api,
        })
    }

    fn belongs_to(
        &self,
        principal: &VerifiedWebPrincipal,
        descriptor: &WorkspaceConnectionDescriptor,
    ) -> bool {
        self.descriptor_version == descriptor.descriptor_version
            && self.workspace_id == descriptor.workspace_id
            && self.organization_id == descriptor.organization_id
            && descriptor_has_candidate(descriptor, &self.candidate)
            && self.principal.matches(principal)
    }
}

fn descriptor_has_candidate(
    descriptor: &WorkspaceConnectionDescriptor,
    candidate: &WorkspaceConnectionCandidate,
) -> bool {
    descriptor.candidates.contains(candidate)
}

/// Non-secret identity fields that scope one resolved API handle to one verified principal.
///
/// 用于限制 API handle 的非敏感已验证主体字段。
#[derive(Clone, PartialEq, Eq)]
struct VerifiedPrincipalBinding {
    issuer: String,
    subject: String,
    organization_id: String,
    expires_at_unix_ms: i64,
}

impl VerifiedPrincipalBinding {
    fn from_principal(
        principal: &VerifiedWebPrincipal,
    ) -> Result<Self, WorkspaceApiResolutionError> {
        let identity = principal.identity();
        if identity.issuer.trim().is_empty()
            || identity.subject.trim().is_empty()
            || principal.organization_id().trim().is_empty()
            || principal.expires_at_unix_ms() <= 0
        {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }
        Ok(Self {
            issuer: identity.issuer.clone(),
            subject: identity.subject.clone(),
            organization_id: principal.organization_id().to_string(),
            expires_at_unix_ms: principal.expires_at_unix_ms(),
        })
    }

    fn matches(&self, principal: &VerifiedWebPrincipal) -> bool {
        self.matches_fields(
            &principal.identity().issuer,
            &principal.identity().subject,
            principal.organization_id(),
            principal.expires_at_unix_ms(),
        )
    }

    fn matches_fields(
        &self,
        issuer: &str,
        subject: &str,
        organization_id: &str,
        expires_at_unix_ms: i64,
    ) -> bool {
        self.issuer == issuer
            && self.subject == subject
            && self.organization_id == organization_id
            && self.expires_at_unix_ms == expires_at_unix_ms
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
            .field("principal_bound", &true)
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
    async fn catalog_for_request(
        &self,
        _principal: &VerifiedWebPrincipal,
        _access_token: &str,
        _workspace_id: &str,
    ) -> Result<crate::ProductOperationCatalog, WorkspaceGatewayError> {
        Err(WorkspaceGatewayError::Unavailable)
    }

    async fn invoke(
        &self,
        principal: &VerifiedWebPrincipal,
        access_token: &str,
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
            .resolve(principal, access_token, &descriptor)
            .await
            .map_err(map_resolution_error)?;
        if !binding.belongs_to(principal, &descriptor) {
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
    identity: &cy_workspace_control_plane::workspace_v1::UserIdentityRef,
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
            Some(workspace_api_request::Request::ProductApiV2(invocation))
                if is_valid_wire_owner_id(&invocation.owner_id)
                    && is_valid_wire_operation_id(&invocation.operation_id)
                    && invocation.json_body.len() <= crate::MAX_JSON_BODY_BYTES
                    && invocation.resource_id.len() <= 512
                    && invocation.idempotency_key.len() <= 200
        )
    {
        return Err(WorkspaceGatewayError::InvalidResponse);
    }
    Ok(())
}

fn is_valid_wire_owner_id(value: &str) -> bool {
    let mut characters = value.bytes();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && value.len() <= 63
        && characters.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_valid_wire_operation_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
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

    use cy_workspace_control_plane::workspace_v1::{
        workspace_api_request, GetWorkspaceOperationRequest, WorkspaceApiRequest,
        WorkspaceConnectionCandidate, WorkspaceConnectionDescriptor,
    };
    use cy_workspace_control_plane::{WorkspaceDirectory, WorkspaceDirectoryError};
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
            user: &cy_workspace_control_plane::workspace_v1::UserIdentityRef,
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
            _user: &cy_workspace_control_plane::workspace_v1::UserIdentityRef,
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

    fn identity() -> cy_workspace_control_plane::workspace_v1::UserIdentityRef {
        cy_workspace_control_plane::workspace_v1::UserIdentityRef {
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
        let mut second = descriptor("workspace-b", "org-1");
        second.candidates[0].provider_id = "other-relay-provider".to_string();
        let mut unlisted_candidate = second.candidates[0].clone();
        unlisted_candidate.provider_id = "unlisted-provider".to_string();

        assert!(descriptor_has_candidate(&first, &first.candidates[0]));
        assert!(!descriptor_has_candidate(&first, &second.candidates[0]));
        assert!(!descriptor_has_candidate(&first, &unlisted_candidate));
    }

    #[test]
    fn principal_binding_rejects_cross_user_org_and_expiry_mismatch() {
        let binding = VerifiedPrincipalBinding {
            issuer: "https://issuer.example/tenant/v2.0".to_string(),
            subject: "user-a".to_string(),
            organization_id: "org-a".to_string(),
            expires_at_unix_ms: 1_900_000_000_000,
        };

        assert!(binding.matches_fields(
            "https://issuer.example/tenant/v2.0",
            "user-a",
            "org-a",
            1_900_000_000_000,
        ));
        assert!(!binding.matches_fields(
            "https://issuer.example/tenant/v2.0",
            "user-b",
            "org-a",
            1_900_000_000_000,
        ));
        assert!(!binding.matches_fields(
            "https://issuer.example/tenant/v2.0",
            "user-a",
            "org-b",
            1_900_000_000_000,
        ));
        assert!(!binding.matches_fields(
            "https://issuer.example/tenant/v2.0",
            "user-a",
            "org-a",
            1_900_000_000_001,
        ));
    }

    #[test]
    fn gateway_accepts_only_v2_product_requests_without_caller_or_url_fields() {
        let valid = WorkspaceApiRequest {
            request_id: "request-1".to_string(),
            workspace_id: "workspace-a".to_string(),
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            request: Some(workspace_api_request::Request::ProductApiV2(
                cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2 {
                    owner_id: "catalyst".into(),
                    operation_id: "workspaceListDatasets".into(),
                    ..Default::default()
                },
            )),
        };
        assert!(validate_gateway_request(&valid).is_ok());

        let non_product = WorkspaceApiRequest {
            request: Some(workspace_api_request::Request::GetOperation(
                GetWorkspaceOperationRequest::default(),
            )),
            ..valid.clone()
        };
        assert_eq!(
            validate_gateway_request(&non_product),
            Err(WorkspaceGatewayError::InvalidResponse)
        );

        let invalid_owner = WorkspaceApiRequest {
            request: Some(workspace_api_request::Request::ProductApiV2(
                cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2 {
                    owner_id: "../catalyst".into(),
                    operation_id: "workspaceListDatasets".into(),
                    ..Default::default()
                },
            )),
            ..valid
        };
        assert_eq!(
            validate_gateway_request(&invalid_owner),
            Err(WorkspaceGatewayError::InvalidResponse)
        );
    }
}
