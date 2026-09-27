//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/host.rs             ║
//! ║ Module: cy_workspace_web_bff::host                                 ║
//! ║ Role: Compose real identity, Directory, catalog, and Relay providers.║
//! ║                                                                    ║
//! ║ 模块职责：装配真实身份、Directory、合同 catalog 与 Relay provider。    ║
//! ╚══════════════════════════════════════════════════════════════════════╝

use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::routing::get;
use axum::Router;
use cy_proto::google::rpc::Status as RpcStatus;
use cy_workspace_fabric::workspace_v1::{
    workspace_api_request, workspace_api_response, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceConnectionDescriptor,
};
use cy_workspace_fabric::{
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, FrontendRelayClient,
    FrontendRelayClientConfig, FrontendRelayClientError, PostgresWorkspaceDirectory,
    VerifiedWebPrincipal, WebIdentityDirectory, WebIdentityDirectoryError, WebPrincipalVerifier,
    WebRelaySessionIssuer, WorkspaceApi, WorkspaceCallerContext, WorkspaceCallerPrincipal,
    WorkspaceDirectory, WORKSPACE_MEMBER_ROLE,
};
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{Response, StatusCode};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroize;

use cy_workspace_web_bff::{
    load_product_operation_catalog_from_environment, router as bff_router,
    FabricWorkspaceProductGateway, WebBffConfig, WebBffState, WorkspaceApiBinding,
    WorkspaceApiResolutionError, WorkspaceApiResolver,
};

const MAX_CERTIFICATE_FILE_BYTES: u64 = 256 * 1024;
const CSRF_KEY_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE";
const TENANT_ID_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID";
const AUDIENCE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE";
const CLIENT_ORIGIN_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN";
const RELAY_ENDPOINT_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_RELAY_ENDPOINT";
const RELAY_SERVER_NAME_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_RELAY_SERVER_NAME";
const RELAY_CA_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_RELAY_CA_FILE";
const RELAY_CLIENT_CERT_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_CERT_FILE";
const RELAY_CLIENT_KEY_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_KEY_FILE";
const HANDOFF_ISSUER_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_HANDOFF_ISSUER";
const HANDOFF_AUDIENCE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_HANDOFF_AUDIENCE";
const HANDOFF_SIGNING_SEED_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_HANDOFF_SIGNING_SEED_FILE";

/// Fixed startup failures which never include credential values or backend diagnostics.
///
/// 固定启动错误，不包含 credential 或 backend 诊断细节。
#[derive(Debug, Error)]
pub(crate) enum HostStartupError {
    #[error("required Workspace Web BFF configuration is missing or invalid")]
    Configuration,
    #[error("Workspace Directory could not be initialized")]
    Directory,
    #[error("Workspace Web identity verifier could not be initialized")]
    Identity,
    #[error("Workspace Relay transport configuration is invalid")]
    Relay,
    #[error("trusted Product contract bundle could not be loaded")]
    ProductCatalog,
    #[error("Workspace Web BFF application state could not be initialized")]
    Application,
    #[error("Workspace Web BFF HTTP listener failed")]
    Listener(#[from] io::Error),
}

#[derive(Clone)]
struct RelaySettings {
    endpoint: String,
    server_name: String,
    ca_certificate: Vec<u8>,
    client_certificate: Vec<u8>,
    client_key: Vec<u8>,
}

impl RelaySettings {
    fn client_config(&self) -> Result<FrontendRelayClientConfig, HostStartupError> {
        FrontendRelayClientConfig::new(
            self.endpoint.clone(),
            self.server_name.clone(),
            self.ca_certificate.clone(),
            self.client_certificate.clone(),
            self.client_key.clone(),
        )
        .map_err(|_| HostStartupError::Relay)
    }
}

impl Drop for RelaySettings {
    fn drop(&mut self) {
        self.client_key.zeroize();
    }
}

struct HostSettings {
    client_origin: String,
    csrf_mac_key: Secret32,
    tenant_id: Uuid,
    audience: String,
    relay: RelaySettings,
    handoff_issuer: String,
    handoff_audience: String,
    handoff_signing_seed: Secret32,
}

struct Secret32([u8; 32]);

impl Secret32 {
    fn duplicate(&self) -> [u8; 32] {
        self.0
    }
}

impl Drop for Secret32 {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl HostSettings {
    fn from_environment() -> Result<Self, HostStartupError> {
        let client_origin = required_text(CLIENT_ORIGIN_ENV)?;
        let tenant_id = required_text(TENANT_ID_ENV)?
            .parse::<Uuid>()
            .map_err(|_| HostStartupError::Configuration)?;
        let audience = required_text(AUDIENCE_ENV)?;
        let csrf_mac_key = read_secret_32(CSRF_KEY_FILE_ENV)?;
        let handoff_issuer = required_text(HANDOFF_ISSUER_ENV)?;
        let handoff_audience = required_text(HANDOFF_AUDIENCE_ENV)?;
        let handoff_signing_seed = read_secret_32(HANDOFF_SIGNING_SEED_FILE_ENV)?;

        let relay = RelaySettings {
            endpoint: required_text(RELAY_ENDPOINT_ENV)?,
            server_name: required_text(RELAY_SERVER_NAME_ENV)?,
            ca_certificate: read_regular_file(
                RELAY_CA_FILE_ENV,
                MAX_CERTIFICATE_FILE_BYTES,
                false,
            )?,
            client_certificate: read_regular_file(
                RELAY_CLIENT_CERT_FILE_ENV,
                MAX_CERTIFICATE_FILE_BYTES,
                false,
            )?,
            client_key: read_regular_file(
                RELAY_CLIENT_KEY_FILE_ENV,
                MAX_CERTIFICATE_FILE_BYTES,
                true,
            )?,
        };

        // Validate fixed HTTPS/SNI/mTLS settings before any listener can accept traffic.
        drop(relay.client_config()?);
        let mut seed_for_validation = handoff_signing_seed.duplicate();
        let issuer_validation = WebRelaySessionIssuer::new(
            handoff_issuer.clone(),
            handoff_audience.clone(),
            seed_for_validation,
        )
        .map_err(|_| HostStartupError::Relay);
        seed_for_validation.zeroize();
        issuer_validation?;

        if client_origin.is_empty() || audience.is_empty() || tenant_id.is_nil() {
            return Err(HostStartupError::Configuration);
        }

        Ok(Self {
            client_origin,
            csrf_mac_key,
            tenant_id,
            audience,
            relay,
            handoff_issuer,
            handoff_audience,
            handoff_signing_seed,
        })
    }
}

/// Compose every required production dependency before opening the HTTP listener.
///
/// PostgreSQL reachability, OIDC verifier setup, the complete pinned Product bundle,
/// exact Relay TLS routing, and the signed handoff issuer are required. Relay sessions
/// are created only after a real inbound token has produced a `VerifiedWebPrincipal`;
/// no synthetic user is used for a readiness probe.
///
/// 在打开 HTTP listener 前装配所有必要生产依赖。Relay session 只会在真实 token 验证后创建，不使用合成用户探测。
pub(crate) async fn compose() -> Result<Router, HostStartupError> {
    let settings = HostSettings::from_environment()?;
    let mut csrf_mac_key = settings.csrf_mac_key.duplicate();
    let web_config = WebBffConfig::new(settings.client_origin.clone(), csrf_mac_key)
        .map_err(|_| HostStartupError::Application);
    csrf_mac_key.zeroize();
    let web_config = web_config?;

    let directory = Arc::new(
        PostgresWorkspaceDirectory::connect_from_environment()
            .await
            .map_err(|_| HostStartupError::Directory)?,
    );
    let identity_directory: Arc<dyn WebIdentityDirectory> = Arc::new(PostgresIdentityDirectory {
        directory: Arc::clone(&directory),
    });
    let identity_config =
        AzureAdWebIdentityConfig::new(settings.tenant_id, settings.audience.clone())
            .map_err(|_| HostStartupError::Identity)?;
    let principal_verifier: Arc<dyn WebPrincipalVerifier> = Arc::new(
        AzureAdWebPrincipalVerifier::new(identity_config, identity_directory)
            .map_err(|_| HostStartupError::Identity)?,
    );
    let mut signing_seed = settings.handoff_signing_seed.duplicate();
    let handoff_issuer = Arc::new(
        WebRelaySessionIssuer::new(
            settings.handoff_issuer.clone(),
            settings.handoff_audience.clone(),
            signing_seed,
        )
        .map_err(|_| HostStartupError::Relay)?,
    );
    signing_seed.zeroize();

    let resolver: Arc<dyn WorkspaceApiResolver> = Arc::new(PrincipalScopedRelayResolver {
        relay: settings.relay.clone(),
        handoff_issuer,
    });
    let workspace_directory: Arc<dyn WorkspaceDirectory> = directory;
    let workspace_gateway = Arc::new(FabricWorkspaceProductGateway::new(
        Arc::clone(&workspace_directory),
        resolver,
    ));
    let product_catalog = load_product_operation_catalog_from_environment()
        .map_err(|_| HostStartupError::ProductCatalog)?;
    let state = WebBffState::new(
        web_config,
        principal_verifier,
        workspace_directory,
        workspace_gateway,
        product_catalog,
    )
    .map_err(|_| HostStartupError::Application)?;

    Ok(with_probes(bff_router(Arc::new(state))))
}

/// Start liveness after composition while keeping readiness closed until external trust paths
/// can be health-checked without fabricating a user identity.
///
/// Composition verifies local configuration, PostgreSQL reachability, and the pinned contract
/// bundle. It cannot prove AAD JWKS availability or Relay service trust without a real user
/// token, so `readyz` remains 503 and this build is not eligible for live ingress.
///
/// 装配后可报告进程存活；在不伪造用户身份的前提下验证 AAD JWKS 与 Relay 服务信任前，readiness 始终关闭。
fn with_probes(application: Router) -> Router {
    Router::new()
        .merge(application)
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
}

async fn health() -> Response<Body> {
    probe_response(StatusCode::OK, br#"{"status":"live"}"#)
}

async fn ready() -> Response<Body> {
    probe_response(
        StatusCode::SERVICE_UNAVAILABLE,
        br#"{"status":"not_ready"}"#,
    )
}

fn probe_response(status: StatusCode, body: &'static [u8]) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, http::HeaderValue::from_static("no-store"));
    response
}

struct PostgresIdentityDirectory {
    directory: Arc<PostgresWorkspaceDirectory>,
}

#[async_trait]
impl WebIdentityDirectory for PostgresIdentityDirectory {
    async fn organizations_for_verified_identity(
        &self,
        identity: &cy_workspace_fabric::workspace_v1::UserIdentityRef,
    ) -> Result<Vec<String>, WebIdentityDirectoryError> {
        self.directory
            .organizations_for_verified_identity(identity)
            .await
            .map_err(|error| match error {
                cy_workspace_fabric::DurableDirectoryError::InvalidIdentity
                | cy_workspace_fabric::DurableDirectoryError::NoMembershipMapping => {
                    WebIdentityDirectoryError::MissingMapping
                }
                cy_workspace_fabric::DurableDirectoryError::AmbiguousOrganizations => {
                    WebIdentityDirectoryError::AmbiguousMapping
                }
                _ => WebIdentityDirectoryError::Unavailable,
            })
    }
}

struct PrincipalScopedRelayResolver {
    relay: RelaySettings,
    handoff_issuer: Arc<WebRelaySessionIssuer>,
}

#[async_trait]
impl WorkspaceApiResolver for PrincipalScopedRelayResolver {
    async fn resolve(
        &self,
        principal: &VerifiedWebPrincipal,
        descriptor: &WorkspaceConnectionDescriptor,
    ) -> Result<WorkspaceApiBinding, WorkspaceApiResolutionError> {
        let config = self
            .relay
            .client_config()
            .map_err(|_| WorkspaceApiResolutionError::NotConfigured)?;
        let mut transport =
            FrontendRelayClient::connect(config, principal, self.handoff_issuer.as_ref())
                .await
                .map_err(map_relay_resolution_error)?;
        let discovered = transport
            .discover_workspaces()
            .await
            .map_err(map_relay_resolution_error)?;
        let mut exact = discovered
            .iter()
            .filter(|candidate| candidate.workspace_id == descriptor.workspace_id);
        let discovered_descriptor = exact
            .next()
            .ok_or(WorkspaceApiResolutionError::NotConfigured)?;
        if exact.next().is_some() || discovered_descriptor != descriptor {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }
        let mut candidates = descriptor.candidates.iter().filter(|candidate| {
            candidate.mode == cy_proto::core_v1::ConnectivityMode::Relay as i32
                && candidate.connection_uri == self.relay.endpoint
                && candidate.server_name == self.relay.server_name
        });
        let candidate = candidates
            .next()
            .ok_or(WorkspaceApiResolutionError::NotConfigured)?;
        if candidates.next().is_some() {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }

        let api: Arc<dyn WorkspaceApi> = Arc::new(PrincipalScopedWorkspaceApi {
            transport: Mutex::new(transport),
            identity: principal.identity().clone(),
            organization_id: principal.organization_id().to_owned(),
            workspace_id: descriptor.workspace_id.clone(),
            expires_at_unix_ms: principal.expires_at_unix_ms(),
        });
        WorkspaceApiBinding::for_candidate(principal, descriptor, candidate, api)
    }
}

struct PrincipalScopedWorkspaceApi {
    transport: Mutex<FrontendRelayClient>,
    identity: cy_workspace_fabric::workspace_v1::UserIdentityRef,
    organization_id: String,
    workspace_id: String,
    expires_at_unix_ms: i64,
}

#[async_trait]
impl WorkspaceApi for PrincipalScopedWorkspaceApi {
    async fn handle_authenticated(
        &self,
        request: WorkspaceApiRequest,
        caller: WorkspaceCallerContext,
    ) -> WorkspaceApiResponse {
        let request_id = request.request_id.clone();
        let caller_matches = matches!(
            caller.principal(),
            WorkspaceCallerPrincipal::User(user) if user == &self.identity
        ) && caller.organization_id() == self.organization_id
            && caller.workspace_id() == self.workspace_id
            && caller.is_member()
            && caller.roles().contains(WORKSPACE_MEMBER_ROLE)
            && request.workspace_id == self.workspace_id
            && matches!(
                request.request.as_ref(),
                Some(workspace_api_request::Request::ProductApi(_))
            );
        if !caller_matches {
            return workspace_error(request_id, 7, "WORKSPACE_CALLER_CONTEXT_REQUIRED");
        }
        if self.expires_at_unix_ms <= unix_now_ms().unwrap_or(i64::MAX) {
            return workspace_error(request_id, 16, "WORKSPACE_CALLER_EXPIRED");
        }

        match self.transport.lock().await.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                let (code, message) = match error {
                    FrontendRelayClientError::Timeout => (4, "WORKSPACE_RELAY_REQUEST_TIMEOUT"),
                    FrontendRelayClientError::RelayUnavailable
                    | FrontendRelayClientError::SessionExpired
                    | FrontendRelayClientError::CredentialUnavailable => {
                        (14, "WORKSPACE_RELAY_UNAVAILABLE")
                    }
                    _ => (13, "WORKSPACE_RELAY_RESPONSE_INVALID"),
                };
                workspace_error(request_id, code, message)
            }
        }
    }
}

fn workspace_error(request_id: String, code: i32, message: &'static str) -> WorkspaceApiResponse {
    WorkspaceApiResponse {
        request_id,
        outcome: Some(workspace_api_response::Outcome::Error(RpcStatus {
            code,
            message: message.to_owned(),
            details: Vec::new(),
        })),
    }
}

fn map_relay_resolution_error(error: FrontendRelayClientError) -> WorkspaceApiResolutionError {
    match error {
        FrontendRelayClientError::Configuration => WorkspaceApiResolutionError::NotConfigured,
        FrontendRelayClientError::Timeout
        | FrontendRelayClientError::RelayUnavailable
        | FrontendRelayClientError::CredentialUnavailable
        | FrontendRelayClientError::SessionExpired => WorkspaceApiResolutionError::Unavailable,
        FrontendRelayClientError::DescriptorInvalid
        | FrontendRelayClientError::WorkspaceNotDiscovered
        | FrontendRelayClientError::RequestInvalid
        | FrontendRelayClientError::ResponseInvalid
        | FrontendRelayClientError::CorrelationUnavailable
        | FrontendRelayClientError::ClockUnavailable => WorkspaceApiResolutionError::InvalidBinding,
    }
}

fn unix_now_ms() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

fn required_text(name: &'static str) -> Result<String, HostStartupError> {
    let value = env::var(name).map_err(|_| HostStartupError::Configuration)?;
    if value.trim() != value
        || value.is_empty()
        || value.len() > 2048
        || value.chars().any(char::is_control)
    {
        return Err(HostStartupError::Configuration);
    }
    Ok(value)
}

fn read_secret_32(name: &'static str) -> Result<Secret32, HostStartupError> {
    let mut bytes = read_regular_file(name, 32, true)?;
    let secret = bytes
        .as_slice()
        .try_into()
        .map(Secret32)
        .map_err(|_| HostStartupError::Configuration);
    bytes.zeroize();
    secret
}

fn read_regular_file(
    name: &'static str,
    max_bytes: u64,
    private: bool,
) -> Result<Vec<u8>, HostStartupError> {
    let path = PathBuf::from(required_text(name)?);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(HostStartupError::Configuration);
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| HostStartupError::Configuration)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err(HostStartupError::Configuration);
    }
    if metadata.len() > max_bytes {
        return Err(HostStartupError::Configuration);
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(HostStartupError::Configuration);
        }
    }
    #[cfg(not(unix))]
    let _ = private;

    let file = File::open(&path).map_err(|_| HostStartupError::Configuration)?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| HostStartupError::Configuration)?;
    if !opened_metadata.is_file() || opened_metadata.len() > max_bytes {
        return Err(HostStartupError::Configuration);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened_metadata.dev() || metadata.ino() != opened_metadata.ino() {
            return Err(HostStartupError::Configuration);
        }
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| HostStartupError::Configuration)?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes {
        return Err(HostStartupError::Configuration);
    }
    Ok(bytes)
}
