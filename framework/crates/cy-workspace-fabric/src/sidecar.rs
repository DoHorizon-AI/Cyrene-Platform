//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 sidecar.rs                                                      │
//! │  Module: cy_workspace_fabric::sidecar                               │
//! │  Role: Authenticated loopback API for non-Rust Workspace clients.    │
//! │                                                                     │
//! │  模块职责：为非 Rust Workspace client 提供认证的 loopback API。         │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_proto::workspace_local_v1::workspace_sidecar_service_server::WorkspaceSidecarService as WorkspaceSidecarServicePort;
use cy_proto::workspace_local_v1::{
    DiscoverWorkspacesRequest, DiscoverWorkspacesResponse, ExecuteRequest, ExecuteResponse,
    HealthRequest, HealthResponse, WorkspaceSummary,
};
use cy_proto::workspace_v1::{
    workspace_api_request, RelayHello, RelayParticipantRole, UserIdentityRef,
};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::service::Interceptor;
use tonic::{Request, Response, Status};

use crate::{
    connect_discovered_workspace, connect_relay_session, validate_descriptor, RelayClientConfig,
};

const MAX_CONCURRENT_REMOTE_CALLS: usize = 16;
const REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(35);
const REMOTE_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const DIRECT_SELECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Safe, stable errors for invalid or unavailable sidecar credential files.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SidecarConfigurationError {
    #[error("SIDECAR_FILE_PATH_MUST_BE_ABSOLUTE")]
    RelativePath,
    #[error("SIDECAR_SECRET_FILE_UNAVAILABLE")]
    SecretFileUnavailable,
    #[error("SIDECAR_SECRET_FILE_PERMISSIONS_INVALID")]
    UnsafePermissions,
    #[error("SIDECAR_CREDENTIAL_BUNDLE_INVALID")]
    InvalidCredentialBundle,
    #[error("SIDECAR_LOCAL_TOKEN_INVALID")]
    InvalidLocalToken,
    #[error("SIDECAR_LOCAL_TOKEN_MUST_DIFFER_FROM_SESSION_CREDENTIAL")]
    CredentialDomainCollision,
    #[error("SIDECAR_SECURE_FILE_PERMISSIONS_UNSUPPORTED")]
    SecurePermissionsUnsupported,
}

/// Relay-side user identity and mTLS material provisioned by an external issuer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialBundleFile {
    relay_endpoint: String,
    relay_server_name: String,
    relay_ca_certificate_file: PathBuf,
    client_certificate_file: PathBuf,
    client_key_file: PathBuf,
    session_credential: String,
    user_issuer: String,
    user_subject: String,
    organization_id: String,
    allowed_workspace_ids: BTreeSet<String>,
    allowed_operations: BTreeSet<WorkspaceOperationPermission>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum WorkspaceOperationPermission {
    StartOperation,
    GetOperation,
}

struct LoadedCredentials {
    relay: RelayClientConfig,
    hello: RelayHello,
    user: UserIdentityRef,
    allowed_workspace_ids: BTreeSet<String>,
    allowed_operations: BTreeSet<WorkspaceOperationPermission>,
}

/// Workspace API proxy that consumes, but never issues, identity credentials.
///
/// Credential bundles are re-read for each remote call so an external identity
/// service can rotate them by atomically replacing the protected file. No local
/// handler accepts or returns relay endpoints or private routing candidates.
pub struct WorkspaceSidecar {
    credential_bundle_path: PathBuf,
    local_bearer_token: Arc<Vec<u8>>,
    remote_slots: Arc<Semaphore>,
}

impl WorkspaceSidecar {
    /// Load and validate the external credential bundle before opening a listener.
    pub fn new(
        credential_bundle_path: impl Into<PathBuf>,
        local_bearer: &LocalBearerInterceptor,
    ) -> Result<Self, SidecarConfigurationError> {
        let sidecar = Self {
            credential_bundle_path: credential_bundle_path.into(),
            local_bearer_token: Arc::clone(&local_bearer.expected_token),
            remote_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_REMOTE_CALLS)),
        };
        sidecar.load_authorized_credentials()?;
        Ok(sidecar)
    }

    /// Reload credentials and reject reuse of the remote session secret locally.
    fn load_authorized_credentials(&self) -> Result<LoadedCredentials, SidecarConfigurationError> {
        let credentials = load_credentials(&self.credential_bundle_path)?;
        if constant_time_eq(
            self.local_bearer_token.as_slice(),
            credentials.hello.session_credential.as_bytes(),
        ) {
            return Err(SidecarConfigurationError::CredentialDomainCollision);
        }
        Ok(credentials)
    }
}

/// Loopback gRPC authentication using a separately provisioned local bearer token.
#[derive(Clone)]
pub struct LocalBearerInterceptor {
    expected_token: Arc<Vec<u8>>,
}

impl LocalBearerInterceptor {
    /// Read a local caller token from a private file and validate its shape.
    pub fn from_file(path: &Path) -> Result<Self, SidecarConfigurationError> {
        require_absolute(path)?;
        ensure_secret_file_permissions(path, true)?;
        let token = fs::read(path).map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
        let token = token.strip_suffix(b"\n").unwrap_or(&token);
        let token = token.strip_suffix(b"\r").unwrap_or(token);
        if !(32..=256).contains(&token.len())
            || !token
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(SidecarConfigurationError::InvalidLocalToken);
        }
        Ok(Self {
            expected_token: Arc::new(token.to_vec()),
        })
    }
}

impl Interceptor for LocalBearerInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let authorized = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
            .is_some_and(|provided| constant_time_eq(provided, self.expected_token.as_slice()));
        if authorized {
            Ok(request)
        } else {
            Err(Status::unauthenticated("SIDECAR_LOCAL_AUTH_REQUIRED"))
        }
    }
}

#[tonic::async_trait]
impl WorkspaceSidecarServicePort for WorkspaceSidecar {
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            local_ready: self.load_authorized_credentials().is_ok(),
        }))
    }

    async fn discover_workspaces(
        &self,
        _request: Request<DiscoverWorkspacesRequest>,
    ) -> Result<Response<DiscoverWorkspacesResponse>, Status> {
        let _remote_slot = self
            .acquire_remote_slot()
            .ok_or_else(|| Status::resource_exhausted("SIDECAR_REMOTE_CAPACITY_REACHED"))?;
        let credentials = self
            .load_authorized_credentials()
            .map_err(|_| Status::failed_precondition("SIDECAR_CREDENTIALS_UNAVAILABLE"))?;
        let mut relay = tokio::time::timeout(
            REMOTE_CONNECT_TIMEOUT,
            connect_relay_session(&credentials.relay, credentials.hello.clone()),
        )
        .await
        .map_err(|_| Status::unavailable("WORKSPACE_FABRIC_TIMEOUT"))?
        .map_err(|_| Status::unavailable("WORKSPACE_FABRIC_UNAVAILABLE"))?;
        let descriptors = tokio::time::timeout(
            REMOTE_CALL_TIMEOUT,
            relay.discover(
                uuid::Uuid::new_v4().to_string(),
                credentials.user,
                credentials.hello.organization_id.clone(),
            ),
        )
        .await
        .map_err(|_| Status::unavailable("WORKSPACE_DISCOVERY_TIMEOUT"))?
        .map_err(|_| Status::unavailable("WORKSPACE_DISCOVERY_FAILED"))?;

        let now_unix_ms = now_unix_ms();
        let mut workspaces = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            if descriptor.organization_id != credentials.hello.organization_id
                || validate_descriptor(&descriptor, now_unix_ms).is_err()
            {
                return Err(Status::internal("WORKSPACE_DESCRIPTOR_INVALID"));
            }
            workspaces.push(WorkspaceSummary {
                workspace_id: descriptor.workspace_id,
                display_name: descriptor.display_name,
            });
        }
        Ok(Response::new(DiscoverWorkspacesResponse {
            workspaces: workspaces
                .into_iter()
                .filter(|workspace| {
                    credentials
                        .allowed_workspace_ids
                        .contains(&workspace.workspace_id)
                })
                .collect(),
        }))
    }

    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        let _remote_slot = self
            .acquire_remote_slot()
            .ok_or_else(|| Status::resource_exhausted("SIDECAR_REMOTE_CAPACITY_REACHED"))?;
        let api_request = request
            .into_inner()
            .workspace_request
            .ok_or_else(|| Status::invalid_argument("WORKSPACE_REQUEST_INCOMPLETE"))?;
        if api_request.request_id.trim().is_empty()
            || api_request.workspace_id.trim().is_empty()
            || api_request.request.is_none()
        {
            return Err(Status::invalid_argument("WORKSPACE_REQUEST_INCOMPLETE"));
        }

        let credentials = self
            .load_authorized_credentials()
            .map_err(|_| Status::failed_precondition("SIDECAR_CREDENTIALS_UNAVAILABLE"))?;
        if !credentials
            .allowed_workspace_ids
            .contains(&api_request.workspace_id)
        {
            return Err(Status::permission_denied("SIDECAR_WORKSPACE_NOT_ALLOWED"));
        }
        if !operation_is_allowed(
            api_request.request.as_ref(),
            &credentials.allowed_operations,
        ) {
            return Err(Status::permission_denied("SIDECAR_OPERATION_NOT_ALLOWED"));
        }
        let mut relay = tokio::time::timeout(
            REMOTE_CONNECT_TIMEOUT,
            connect_relay_session(&credentials.relay, credentials.hello.clone()),
        )
        .await
        .map_err(|_| Status::unavailable("WORKSPACE_FABRIC_TIMEOUT"))?
        .map_err(|_| Status::unavailable("WORKSPACE_FABRIC_UNAVAILABLE"))?;
        let descriptors = tokio::time::timeout(
            REMOTE_CALL_TIMEOUT,
            relay.discover(
                uuid::Uuid::new_v4().to_string(),
                credentials.user,
                credentials.hello.organization_id.clone(),
            ),
        )
        .await
        .map_err(|_| Status::unavailable("WORKSPACE_DISCOVERY_TIMEOUT"))?
        .map_err(|_| Status::unavailable("WORKSPACE_DISCOVERY_FAILED"))?;
        let descriptor = descriptors
            .into_iter()
            .find(|candidate| candidate.workspace_id == api_request.workspace_id)
            .ok_or_else(|| Status::not_found("WORKSPACE_NOT_DISCOVERABLE"))?;
        if descriptor.organization_id != credentials.hello.organization_id
            || validate_descriptor(&descriptor, now_unix_ms()).is_err()
        {
            return Err(Status::internal("WORKSPACE_DESCRIPTOR_INVALID"));
        }

        let mut connection = tokio::time::timeout(
            DIRECT_SELECTION_TIMEOUT,
            connect_discovered_workspace(&descriptor, credentials.hello, &credentials.relay, relay),
        )
        .await
        .map_err(|_| Status::unavailable("WORKSPACE_ROUTE_TIMEOUT"))?
        .map_err(|_| Status::unavailable("WORKSPACE_ROUTE_UNAVAILABLE"))?;
        let response = tokio::time::timeout(REMOTE_CALL_TIMEOUT, connection.execute(api_request))
            .await
            .map_err(|_| Status::unavailable("WORKSPACE_REQUEST_TIMEOUT"))?
            .map_err(|_| Status::unavailable("WORKSPACE_REQUEST_FAILED"))?;
        Ok(Response::new(ExecuteResponse {
            workspace_response: Some(response),
        }))
    }
}

impl WorkspaceSidecar {
    fn acquire_remote_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.remote_slots.clone().try_acquire_owned().ok()
    }
}

fn load_credentials(path: &Path) -> Result<LoadedCredentials, SidecarConfigurationError> {
    require_absolute(path)?;
    ensure_secret_file_permissions(path, false)?;
    let serialized =
        fs::read(path).map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
    let bundle: CredentialBundleFile = serde_json::from_slice(&serialized)
        .map_err(|_| SidecarConfigurationError::InvalidCredentialBundle)?;
    if !bundle.relay_endpoint.starts_with("https://")
        || bundle.relay_endpoint.contains(['@', '?', '#'])
        || bundle.relay_server_name.trim().is_empty()
        || bundle.session_credential.trim().is_empty()
        || bundle.session_credential.chars().any(char::is_control)
        || bundle.user_issuer.trim().is_empty()
        || bundle.user_issuer.chars().any(char::is_control)
        || bundle.user_subject.trim().is_empty()
        || bundle.user_subject.chars().any(char::is_control)
        || bundle.organization_id.trim().is_empty()
        || bundle.organization_id.chars().any(char::is_control)
        || bundle.allowed_workspace_ids.is_empty()
        || bundle.allowed_workspace_ids.iter().any(|workspace_id| {
            workspace_id.trim().is_empty()
                || workspace_id == "*"
                || workspace_id.chars().any(char::is_control)
        })
        || bundle.allowed_operations.is_empty()
        || !bundle.relay_ca_certificate_file.is_absolute()
        || !bundle.client_certificate_file.is_absolute()
        || !bundle.client_key_file.is_absolute()
    {
        return Err(SidecarConfigurationError::InvalidCredentialBundle);
    }
    ensure_secret_file_permissions(&bundle.client_key_file, false)?;
    let ca_certificate_pem = fs::read(&bundle.relay_ca_certificate_file)
        .map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
    let client_certificate_pem = fs::read(&bundle.client_certificate_file)
        .map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
    let client_key_pem = fs::read(&bundle.client_key_file)
        .map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
    if ca_certificate_pem.is_empty()
        || client_certificate_pem.is_empty()
        || client_key_pem.is_empty()
    {
        return Err(SidecarConfigurationError::InvalidCredentialBundle);
    }

    let user = UserIdentityRef {
        issuer: bundle.user_issuer,
        subject: bundle.user_subject,
    };
    let relay = RelayClientConfig {
        control_endpoint: bundle.relay_endpoint,
        server_name: bundle.relay_server_name,
        ca_certificate_pem,
        client_certificate_pem,
        client_key_pem,
    };
    let hello = RelayHello {
        role: RelayParticipantRole::Frontend as i32,
        session_credential: bundle.session_credential,
        user: Some(user.clone()),
        organization_id: bundle.organization_id,
        workspace_id: String::new(),
        device: None,
    };
    Ok(LoadedCredentials {
        relay,
        hello,
        user,
        allowed_workspace_ids: bundle.allowed_workspace_ids,
        allowed_operations: bundle.allowed_operations,
    })
}

fn require_absolute(path: &Path) -> Result<(), SidecarConfigurationError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(SidecarConfigurationError::RelativePath)
    }
}

fn operation_is_allowed(
    request: Option<&workspace_api_request::Request>,
    allowed_operations: &BTreeSet<WorkspaceOperationPermission>,
) -> bool {
    let permission = match request {
        Some(workspace_api_request::Request::StartOperation(_)) => {
            WorkspaceOperationPermission::StartOperation
        }
        Some(workspace_api_request::Request::GetOperation(_)) => {
            WorkspaceOperationPermission::GetOperation
        }
        Some(workspace_api_request::Request::ProductApi(_)) | None => return false,
    };
    allowed_operations.contains(&permission)
}

fn ensure_secret_file_permissions(
    path: &Path,
    allow_group_read: bool,
) -> Result<(), SidecarConfigurationError> {
    let metadata =
        fs::metadata(path).map_err(|_| SidecarConfigurationError::SecretFileUnavailable)?;
    if !metadata.is_file() {
        return Err(SidecarConfigurationError::SecretFileUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o7777;
        let allowed = if allow_group_read { 0o640 } else { 0o600 };
        if mode & !allowed != 0 || mode & 0o400 == 0 {
            return Err(SidecarConfigurationError::UnsafePermissions);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = allow_group_read;
        Err(SidecarConfigurationError::SecurePermissionsUnsupported)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let longest = left.len().max(right.len());
    for index in 0..longest {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use tonic::metadata::MetadataValue;

    use super::*;

    fn interceptor() -> LocalBearerInterceptor {
        LocalBearerInterceptor {
            expected_token: Arc::new(b"sidecar-test-token-with-at-least-32-bytes".to_vec()),
        }
    }

    #[cfg(unix)]
    fn write_test_credential_bundle(directory: &Path, session_credential: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let ca_path = directory.join("relay-ca.pem");
        let certificate_path = directory.join("client.pem");
        let key_path = directory.join("client-key.pem");
        let bundle_path = directory.join("credential-bundle.json");
        for path in [&ca_path, &certificate_path, &key_path] {
            fs::write(path, b"test-only nonempty material").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let bundle = serde_json::json!({
            "relay_endpoint": "https://relay.example.invalid",
            "relay_server_name": "relay.example.invalid",
            "relay_ca_certificate_file": ca_path,
            "client_certificate_file": certificate_path,
            "client_key_file": key_path,
            "session_credential": session_credential,
            "user_issuer": "https://identity.example.invalid",
            "user_subject": "subject-1",
            "organization_id": "organization-1",
            "allowed_workspace_ids": ["workspace-1"],
            "allowed_operations": ["get_operation"]
        });
        fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        fs::set_permissions(&bundle_path, fs::Permissions::from_mode(0o600)).unwrap();
        bundle_path
    }

    #[test]
    fn local_bearer_auth_requires_exact_token() {
        let mut interceptor = interceptor();
        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::from_static("Bearer sidecar-test-token-with-at-least-32-bytes"),
        );
        assert!(interceptor.call(request).is_ok());

        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::from_static("Bearer sidecar-test-token-with-at-least-32-byteS"),
        );
        assert_eq!(
            interceptor.call(request).unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
    }

    #[test]
    fn local_bearer_auth_rejects_missing_header() {
        let mut interceptor = interceptor();
        assert_eq!(
            interceptor.call(Request::new(())).unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
    }

    #[test]
    fn credential_bundle_rejects_relative_paths() {
        assert_eq!(
            require_absolute(Path::new("credentials.json")),
            Err(SidecarConfigurationError::RelativePath)
        );
    }

    #[test]
    fn sidecar_fails_closed_when_no_external_bundle_exists() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing-credentials.json");
        let local_bearer = interceptor();
        assert!(matches!(
            WorkspaceSidecar::new(missing, &local_bearer),
            Err(SidecarConfigurationError::SecretFileUnavailable)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_bearer_must_remain_distinct_when_credentials_rotate() {
        let directory = tempfile::tempdir().unwrap();
        let local_bearer = interceptor();
        let bundle_path =
            write_test_credential_bundle(directory.path(), "independent-session-credential");
        let sidecar = WorkspaceSidecar::new(&bundle_path, &local_bearer).unwrap();

        let local_token = std::str::from_utf8(local_bearer.expected_token.as_slice()).unwrap();
        write_test_credential_bundle(directory.path(), local_token);

        assert!(matches!(
            sidecar.load_authorized_credentials(),
            Err(SidecarConfigurationError::CredentialDomainCollision)
        ));
        assert!(matches!(
            WorkspaceSidecar::new(&bundle_path, &local_bearer),
            Err(SidecarConfigurationError::CredentialDomainCollision)
        ));
    }

    #[tokio::test]
    async fn loopback_grpc_rejects_an_unauthenticated_request() {
        use cy_proto::workspace_local_v1::{
            workspace_sidecar_service_client::WorkspaceSidecarServiceClient,
            workspace_sidecar_service_server::WorkspaceSidecarServiceServer, HealthRequest,
        };
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;
        use tokio_stream::wrappers::TcpListenerStream;
        use tonic::service::interceptor::InterceptedService;
        use tonic::transport::{Endpoint, Server};

        let directory = tempfile::tempdir().unwrap();
        let local_bearer = interceptor();
        let sidecar = WorkspaceSidecar {
            credential_bundle_path: directory.path().join("missing-credential-bundle.json"),
            local_bearer_token: Arc::clone(&local_bearer.expected_token),
            remote_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_REMOTE_CALLS)),
        };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, shutdown_signal) = oneshot::channel();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(InterceptedService::new(
                    WorkspaceSidecarServiceServer::new(sidecar),
                    local_bearer,
                ))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown_signal.await;
                })
                .await
                .unwrap();
        });
        let channel = Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = WorkspaceSidecarServiceClient::new(channel);
        let error = client.health(HealthRequest {}).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        let _ = shutdown.send(());
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn credential_bundle_rejects_group_or_world_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credential-bundle.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            ensure_secret_file_permissions(&path, false),
            Err(SidecarConfigurationError::UnsafePermissions)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_token_allows_private_group_read_only_sharing() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("local-token");
        fs::write(&path, b"sidecar-test-token-with-at-least-32-bytes\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(LocalBearerInterceptor::from_file(&path).is_ok());
    }

    #[test]
    fn operation_allowlist_denies_ungranted_workspace_methods() {
        let allowed = BTreeSet::from([WorkspaceOperationPermission::GetOperation]);
        let get_operation = workspace_api_request::Request::GetOperation(
            cy_proto::workspace_v1::GetWorkspaceOperationRequest { operation: None },
        );
        let start_operation = workspace_api_request::Request::StartOperation(
            cy_proto::workspace_v1::StartWorkspaceOperationRequest {
                operation: None,
                input_artifact_uris: Vec::new(),
            },
        );
        let product_api = workspace_api_request::Request::ProductApi(
            cy_proto::workspace_v1::WorkspaceProductApiRequest::default(),
        );

        assert!(operation_is_allowed(Some(&get_operation), &allowed));
        assert!(!operation_is_allowed(Some(&start_operation), &allowed));
        assert!(!operation_is_allowed(Some(&product_api), &allowed));
        assert!(!operation_is_allowed(None, &allowed));
    }
}
