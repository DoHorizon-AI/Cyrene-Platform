//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/host.rs             ║
//! ║ Module: cy_workspace_web_bff::host                                 ║
//! ║ Role: Compose real identity, Directory, and Authority RPC providers.║
//! ║                                                                    ║
//! ║ 模块职责：装配真实身份、Directory 与 Authority RPC provider。       ║
//! ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::routing::get;
use axum::Router;
use cy_proto::cyrene::workspace::authority::v2::workspace_authority_service_client::WorkspaceAuthorityServiceClient;
use cy_proto::workspace_v1::UserIdentityRef;
use cy_workspace_control_plane::{
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, UserCodeKeyRing, WebIdentityDirectory,
    WebIdentityDirectoryError, WebPrincipalVerifier, WorkspaceDirectory,
};
use cy_workspace_postgres_storage::{
    device_enrollment_device_v1_router, webauthn_http_router, DeviceAuthorizationPolicy,
    DeviceAuthorizationPortError, DeviceCertificateIssuer, DeviceCertificatePublicMetadataPort,
    DeviceCertificateRetirementPort, DeviceCertificateRevocationChecker,
    DeviceEnrollmentAuthorizationPort, DeviceEnrollmentAuthorizationService,
    DeviceEnrollmentAuthorizationServiceConfig, DeviceEnrollmentHttpDependencies,
    DurableDirectoryError, PostgresDeviceAuthorizationStore,
    PostgresDeviceEnrollmentRegistrationTransaction, PostgresRestrictedDeviceCa,
    PostgresUserCodeAttemptReservation, PostgresWebAuthnCredentialStore,
    PostgresWebAuthnHttpSessionBindingStore, PostgresWorkspaceDeviceRegistry,
    PostgresWorkspaceDirectory, ProductionDeviceCsrValidator, WebAuthnAuthenticationPort,
    WebAuthnAuthenticationVerifier, WebAuthnCredentialEnrollmentAuthorizer,
    WebAuthnCredentialManagementAction, WebAuthnHttpSessionBindingStore, WebAuthnHttpState,
    WebAuthnVerifierConfig, WebAuthnVerifierSystemClock,
};
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{Response, StatusCode};
use serde_json::json;
use thiserror::Error;
use tokio::time::timeout;
use tonic::transport::Endpoint;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

use cy_workspace_web_bff::{
    router_with_device_approval, with_verified_web_session_routes,
    AuthorityWorkspaceProductGateway, DeviceApprovalDependencies, ProductOperationCatalog,
    WebBffConfig, WebBffState,
};

const CSRF_KEY_FILE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE";
const TENANT_ID_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID";
const AUDIENCE_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE";
const CLIENT_ORIGIN_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN";
const AUTHORITY_UDS_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_AUTHORITY_UDS";
const DEVICE_AUTHORIZATION_USER_CODE_KEY_FILE_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_HMAC_KEY_FILE";
const DEVICE_AUTHORIZATION_VERIFICATION_URI_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_VERIFICATION_URI";
const DEVICE_AUTHORIZATION_USER_CODE_KEY_VERSION_ENV: &str =
    "CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_KEY_VERSION";
const WEBAUTHN_SESSION_BINDING_TIMEOUT: Duration = Duration::from_secs(3);
const READINESS_TIMEOUT: Duration = Duration::from_secs(5);
const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(30);

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
    #[error("WebAuthn HTTP session-binding store could not be initialized")]
    WebAuthnSessionBinding,
    #[error("device enrollment authorization dependencies could not be initialized")]
    DeviceEnrollment,
    #[error("restricted device CA could not be initialized")]
    DeviceCa,
    #[error("Workspace Web BFF application state could not be initialized")]
    Application,
    #[error("Workspace Web BFF HTTP listener failed")]
    Listener(#[from] io::Error),
}

struct HostSettings {
    client_origin: String,
    csrf_mac_key: Secret32,
    tenant_id: Uuid,
    audience: String,
    authority_uds: PathBuf,
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
        let authority_uds = PathBuf::from(required_text(AUTHORITY_UDS_ENV)?);

        if client_origin.is_empty() || audience.is_empty() || tenant_id.is_nil() {
            return Err(HostStartupError::Configuration);
        }

        Ok(Self {
            client_origin,
            csrf_mac_key,
            tenant_id,
            audience,
            authority_uds,
        })
    }
}

/// Compose every required production dependency before opening the HTTP listener.
///
/// PostgreSQL reachability, OIDC verifier setup, and Authority RPC are required.
/// Product catalog snapshots are authenticated and refreshed through Authority for each
/// request; no static local catalog is treated as an authority.
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
    let identity_verifier = Arc::new(
        AzureAdWebPrincipalVerifier::new(identity_config, identity_directory)
            .map_err(|_| HostStartupError::Identity)?,
    );
    let principal_verifier: Arc<dyn WebPrincipalVerifier> = identity_verifier.clone();
    let workspace_directory: Arc<dyn WorkspaceDirectory> = directory.clone();
    let workspace_gateway = Arc::new(AuthorityWorkspaceProductGateway::new(
        Arc::clone(&workspace_directory),
        settings.authority_uds.clone(),
    ));
    let state = Arc::new(
        WebBffState::new(
            web_config,
            principal_verifier,
            workspace_directory,
            workspace_gateway,
            ProductOperationCatalog::unavailable(),
        )
        .map_err(|_| HostStartupError::Application)?,
    );

    let authorization_store = Arc::new(
        tokio::task::spawn_blocking(PostgresDeviceAuthorizationStore::connect_from_environment)
            .await
            .map_err(|_| HostStartupError::DeviceEnrollment)?
            .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let attempt_reservation = Arc::new(
        PostgresUserCodeAttemptReservation::connect_from_environment()
            .await
            .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let registry = Arc::new(
        tokio::task::spawn_blocking(PostgresWorkspaceDeviceRegistry::connect_from_environment)
            .await
            .map_err(|_| HostStartupError::DeviceEnrollment)?
            .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let ca = Arc::new(
        tokio::task::spawn_blocking(PostgresRestrictedDeviceCa::connect_from_environment)
            .await
            .map_err(|_| HostStartupError::DeviceCa)?
            .map_err(|_| HostStartupError::DeviceCa)?,
    );
    tokio::task::spawn_blocking({
        let ca = Arc::clone(&ca);
        move || ca.check_current_crl()
    })
    .await
    .map_err(|_| HostStartupError::DeviceCa)?
    .map_err(|_| HostStartupError::DeviceCa)?;

    let credential_store = Arc::new(
        tokio::task::spawn_blocking(PostgresWebAuthnCredentialStore::connect_from_environment)
            .await
            .map_err(|_| HostStartupError::DeviceEnrollment)?
            .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let session_bindings = Arc::new(
        tokio::time::timeout(
            WEBAUTHN_SESSION_BINDING_TIMEOUT,
            PostgresWebAuthnHttpSessionBindingStore::connect_from_environment(),
        )
        .await
        .map_err(|_| HostStartupError::WebAuthnSessionBinding)?
        .map_err(|_| HostStartupError::WebAuthnSessionBinding)?,
    );

    let policy = DeviceAuthorizationPolicy::default();
    let user_code_keys = user_code_key_ring_from_environment()?;
    let csr_validator = Arc::new(ProductionDeviceCsrValidator);
    let trust_roots = vec![ca.trusted_root_der().to_vec()];
    let revocation_checker = ca.revocation_checker();
    let revocation_checker_for_issuance: Arc<dyn DeviceCertificateRevocationChecker> =
        revocation_checker.clone();
    let issuer: Arc<dyn DeviceCertificateIssuer> = ca.clone();
    let retirement: Arc<dyn DeviceCertificateRetirementPort> = ca.clone();
    let certificate_metadata: Arc<dyn DeviceCertificatePublicMetadataPort> = ca.clone();
    let authorization_directory: Arc<dyn WorkspaceDirectory> = directory.clone();
    let webauthn_origin =
        Url::parse(&settings.client_origin).map_err(|_| HostStartupError::Configuration)?;
    let rp_id = webauthn_origin
        .host_str()
        .ok_or(HostStartupError::Configuration)?
        .to_string();
    let webauthn_config = WebAuthnVerifierConfig::new(rp_id, webauthn_origin)
        .map_err(|_| HostStartupError::DeviceEnrollment)?;
    let credential_store_for_authentication: Arc<
        dyn cy_workspace_postgres_storage::WebAuthnCredentialStore,
    > = credential_store.clone();
    let clock = Arc::new(WebAuthnVerifierSystemClock);
    let webauthn = Arc::new(
        WebAuthnAuthenticationVerifier::new(
            webauthn_config.clone(),
            credential_store_for_authentication,
            clock.clone(),
        )
        .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let webauthn_authentication: Arc<dyn WebAuthnAuthenticationPort> = webauthn;
    let csr_validator_port: Arc<dyn cy_workspace_postgres_storage::DeviceCsrValidator> =
        csr_validator.clone();

    let verification_uri = required_text(DEVICE_AUTHORIZATION_VERIFICATION_URI_ENV)?;
    let registration = Arc::new(
        PostgresDeviceEnrollmentRegistrationTransaction::new(
            Arc::clone(&authorization_store),
            user_code_keys.clone(),
            policy.clone(),
            csr_validator_port.clone(),
            &verification_uri,
        )
        .await
        .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let authorization_service = Arc::new(
        DeviceEnrollmentAuthorizationService::new(DeviceEnrollmentAuthorizationServiceConfig {
            store: Arc::clone(&authorization_store),
            attempt_reservation: Arc::clone(&attempt_reservation),
            directory: authorization_directory,
            webauthn: webauthn_authentication,
            csr_validator: csr_validator_port,
            issuer,
            retirement,
            registry: Arc::clone(&registry),
            certificate_metadata,
            device_certificate_trust_roots_der: trust_roots,
            device_certificate_revocation_checker: revocation_checker_for_issuance,
            user_code_keys: user_code_keys.clone(),
            policy,
        })
        .await
        .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );

    // Recover committed issue/delivery/retirement transitions before routes accept new work.
    authorization_service
        .reconcile_pending_work_once()
        .await
        .map_err(|_| HostStartupError::DeviceEnrollment)?;
    start_reconciliation_worker(Arc::clone(&authorization_service));

    let webauthn_http_state = Arc::new(
        WebAuthnHttpState::production(
            webauthn_config,
            Arc::clone(&credential_store),
            directory.clone(),
            clock,
            Arc::new(TrustedBffCredentialManagementAuthorizer),
            session_bindings.clone(),
            cy_workspace_postgres_storage::WORKSPACE_DEVICE_ENROLLMENT_APPROVE_ROLE,
        )
        .map_err(|_| HostStartupError::DeviceEnrollment)?,
    );
    let webauthn_routes = with_verified_web_session_routes(
        Arc::clone(&state),
        webauthn_http_router(webauthn_http_state),
    );

    let user_enrollment_port: Arc<dyn DeviceEnrollmentAuthorizationPort> =
        authorization_service.clone();
    let approval_service = Arc::new(cy_workspace_web_bff::FabricDeviceApprovalAdapter::new(
        user_enrollment_port,
    ));
    let approval_directory: Arc<dyn WorkspaceDirectory> = directory.clone();
    let session_bindings_for_routes: Arc<dyn WebAuthnHttpSessionBindingStore> =
        session_bindings.clone();
    let approval_dependencies = DeviceApprovalDependencies {
        service: Some(approval_service),
        directory: Some(approval_directory),
        session_bindings: Some(session_bindings_for_routes),
    };

    let device_routes = device_enrollment_device_v1_router(DeviceEnrollmentHttpDependencies {
        registration: Some(registration),
        csr_validator: Some(csr_validator),
        authorization: Some(authorization_service.clone()),
    });
    let application = router_with_device_approval(Arc::clone(&state), approval_dependencies)
        .merge(webauthn_routes)
        .merge(device_routes);
    let readiness = Arc::new(HostReadiness {
        directory,
        identity_verifier,
        authority_uds: settings.authority_uds,
        authorization_store,
        attempt_reservation,
        registry,
        ca,
        credential_store,
        session_bindings,
    });

    Ok(with_probes(application, readiness))
}

/// Start liveness and check each live trust dependency without inventing a user identity.
///
/// Readiness probes current PostgreSQL schemas, signed CRL, OIDC discovery/JWKS, and Authority RPC.
/// Catalog state is authenticated and refreshed for each Product request.
///
/// 启动 liveness 并逐项探测 PostgreSQL schema、签名 CRL、OIDC metadata 与 Authority RPC。
fn with_probes(application: Router, readiness: Arc<HostReadiness>) -> Router {
    Router::new()
        .merge(application)
        .route("/healthz", get(health))
        .route("/readyz", get(move || ready(Arc::clone(&readiness))))
}

async fn health() -> Response<Body> {
    probe_response(StatusCode::OK, "{\"status\":\"live\"}")
}

struct HostReadiness {
    directory: Arc<PostgresWorkspaceDirectory>,
    identity_verifier: Arc<AzureAdWebPrincipalVerifier>,
    authority_uds: PathBuf,
    authorization_store: Arc<PostgresDeviceAuthorizationStore>,
    attempt_reservation: Arc<PostgresUserCodeAttemptReservation>,
    registry: Arc<PostgresWorkspaceDeviceRegistry>,
    ca: Arc<PostgresRestrictedDeviceCa>,
    credential_store: Arc<PostgresWebAuthnCredentialStore>,
    session_bindings: Arc<PostgresWebAuthnHttpSessionBindingStore>,
}

impl HostReadiness {
    async fn checks(&self) -> serde_json::Value {
        let directory = timeout(READINESS_TIMEOUT, self.directory.health_check())
            .await
            .is_ok_and(|result| result.is_ok());
        let identity_provider = timeout(
            READINESS_TIMEOUT,
            self.identity_verifier.check_provider_readiness(),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        let authority_rpc = timeout(
            READINESS_TIMEOUT,
            authority_socket_ready(&self.authority_uds),
        )
        .await
        .is_ok_and(|ready| ready);
        let authorization_database = blocking_readiness({
            let store = Arc::clone(&self.authorization_store);
            move || store.database_time_unix_ms().is_ok()
        })
        .await;
        let user_code_attempt_store =
            timeout(READINESS_TIMEOUT, self.attempt_reservation.health_check())
                .await
                .is_ok_and(|result| result.is_ok());
        let registry = blocking_readiness({
            let registry = Arc::clone(&self.registry);
            move || registry.health_check().is_ok()
        })
        .await;
        let current_signed_crl = blocking_readiness({
            let ca = Arc::clone(&self.ca);
            move || ca.check_current_crl().is_ok()
        })
        .await;
        let webauthn_credentials = blocking_readiness({
            let store = Arc::clone(&self.credential_store);
            move || store.health_check().is_ok()
        })
        .await;
        let webauthn_session_bindings =
            timeout(READINESS_TIMEOUT, self.session_bindings.health_check())
                .await
                .is_ok_and(|result| result.is_ok());
        json!({
            "directoryDatabase": directory,
            "oidcDiscoveryAndJwks": identity_provider,
            "authorityRpc": authority_rpc,
            "deviceAuthorizationDatabase": authorization_database,
            "userCodeAttemptStore": user_code_attempt_store,
            "deviceCertificateRegistry": registry,
            "deviceCaSignedCrl": current_signed_crl,
            "webauthnCredentialStore": webauthn_credentials,
            "webauthnSessionBindingStore": webauthn_session_bindings,
        })
    }
}

struct TrustedBffCredentialManagementAuthorizer;

impl WebAuthnCredentialEnrollmentAuthorizer for TrustedBffCredentialManagementAuthorizer {
    fn authorize_credential_management(
        &self,
        owner: &UserIdentityRef,
        _action: WebAuthnCredentialManagementAction,
    ) -> Result<(), DeviceAuthorizationPortError> {
        if owner.issuer.trim().is_empty()
            || owner.subject.trim().is_empty()
            || owner.issuer.len() > 2048
            || owner.subject.len() > 1024
        {
            return Err(DeviceAuthorizationPortError::Rejected);
        }

        // This private authorizer is reachable only through the browser routes composed below.
        // Those routes require the exact OIDC-verified principal, CSRF pair, and a fresh Directory
        // role check before calling the enrollment service.
        Ok(())
    }
}

fn user_code_key_ring_from_environment() -> Result<UserCodeKeyRing, HostStartupError> {
    let active_version = required_text(DEVICE_AUTHORIZATION_USER_CODE_KEY_VERSION_ENV)?
        .parse::<u32>()
        .map_err(|_| HostStartupError::DeviceEnrollment)?;
    if active_version == 0 {
        return Err(HostStartupError::DeviceEnrollment);
    }
    let key_file = read_secret_32(DEVICE_AUTHORIZATION_USER_CODE_KEY_FILE_ENV)?;
    let mut key = key_file.duplicate();
    if key.iter().all(|byte| *byte == 0) {
        key.zeroize();
        return Err(HostStartupError::DeviceEnrollment);
    }
    let mut keys = BTreeMap::new();
    keys.insert(active_version, key);
    key.zeroize();
    UserCodeKeyRing::new(active_version, keys).map_err(|_| HostStartupError::DeviceEnrollment)
}

fn start_reconciliation_worker(service: Arc<DeviceEnrollmentAuthorizationService>) {
    tokio::spawn(async move {
        loop {
            if service.reconcile_pending_work_once().await.is_err() {
                eprintln!("Workspace device enrollment reconciliation is unavailable");
            }
            tokio::time::sleep(RECONCILIATION_INTERVAL).await;
        }
    });
}

async fn blocking_readiness<F>(check: F) -> bool
where
    F: FnOnce() -> bool + Send + 'static,
{
    timeout(READINESS_TIMEOUT, tokio::task::spawn_blocking(check))
        .await
        .is_ok_and(|result| result.is_ok_and(|ready| ready))
}

async fn authority_socket_ready(socket_path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        let path = socket_path.to_path_buf();
        let endpoint = match Endpoint::try_from("http://[::]:50051") {
            Ok(endpoint) => endpoint,
            Err(_) => return false,
        };
        let channel = match endpoint
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let path = path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, io::Error>(hyper_util::rt::tokio::TokioIo::new(stream))
                }
            }))
            .await
        {
            Ok(channel) => channel,
            Err(_) => return false,
        };
        let mut client = WorkspaceAuthorityServiceClient::new(channel);
        client
            .negotiate_version(tonic::Request::new(
                cy_proto::cyrene::workspace::authority::v2::NegotiateVersionRequest {
                    minimum_version: 2,
                    maximum_version: 2,
                },
            ))
            .await
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        false
    }
}

async fn ready(readiness: Arc<HostReadiness>) -> Response<Body> {
    let checks = timeout(READINESS_TIMEOUT, readiness.checks())
        .await
        .unwrap_or_else(|_| json!({"probe": false}));
    let ready = checks
        .as_object()
        .is_some_and(|values| values.values().all(|value| value.as_bool() == Some(true)));
    let response = json!({
        "status": if ready { "ready" } else { "not_ready" },
        "checks": checks,
    });
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    probe_response(status, response.to_string())
}

fn probe_response(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    let mut response = Response::new(body.into());
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
        identity: &UserIdentityRef,
    ) -> Result<Vec<String>, WebIdentityDirectoryError> {
        self.directory
            .organizations_for_verified_identity(identity)
            .await
            .map_err(|error| match error {
                DurableDirectoryError::InvalidIdentity
                | DurableDirectoryError::NoMembershipMapping => {
                    WebIdentityDirectoryError::MissingMapping
                }
                DurableDirectoryError::AmbiguousOrganizations => {
                    WebIdentityDirectoryError::AmbiguousMapping
                }
                _ => WebIdentityDirectoryError::Unavailable,
            })
    }
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
