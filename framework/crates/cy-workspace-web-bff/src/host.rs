//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/host.rs             ║
//! ║ Module: cy_workspace_web_bff::host                                 ║
//! ║ Role: Compose real identity, Directory, catalog, and Relay providers.║
//! ║                                                                    ║
//! ║ 模块职责：装配真实身份、Directory、合同 catalog 与 Relay provider。    ║
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
use cy_proto::google::rpc::Status as RpcStatus;
use cy_proto::workspace_v1::UserIdentityRef;
use cy_proto::workspace_v1::{
    workspace_api_request, workspace_api_response, WorkspaceApiRequest, WorkspaceApiResponse,
    WorkspaceConnectionDescriptor,
};
use cy_workspace_control_plane::{
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, UserCodeKeyRing, VerifiedWebPrincipal,
    WebIdentityDirectory, WebIdentityDirectoryError, WebPrincipalVerifier, WebRelaySessionIssuer,
    WorkspaceApi, WorkspaceCallerContext, WorkspaceCallerPrincipal, WorkspaceDirectory,
    WORKSPACE_MEMBER_ROLE,
};
use cy_proto::cyrene::workspace::authority::v1::workspace_authority_service_server::WorkspaceAuthorityService;
use cy_proto::cyrene::workspace::authority::v1::*;
use cy_proto::cyrene::workspace::bridge::v1::workspace_frontend_bridge_service_client::WorkspaceFrontendBridgeServiceClient;
use cy_proto::cyrene::workspace::bridge::v1::*;
use cy_workspace_control_plane::authority_service::{
    ContractSnapshotManager, WorkspaceAuthorityServiceImpl,
};
use cy_workspace_postgres_storage::PostgresWorkspaceOutbox;
use tonic::transport::{Channel, Endpoint};
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
use tokio::sync::Mutex;
use tokio::time::timeout;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

use cy_workspace_web_bff::{
    router_with_device_approval,
    with_verified_web_session_routes, DeviceApprovalDependencies, FabricWorkspaceProductGateway,
    WebBffConfig, WebBffState, WorkspaceApiBinding, WorkspaceApiResolutionError,
    WorkspaceApiResolver,
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
    #[error("Workspace Relay transport configuration is invalid")]
    Relay,
    #[error("trusted Product contract bundle could not be loaded")]
    ProductCatalog,
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

#[derive(Clone)]
struct RelaySettings {
    endpoint: String,
    server_name: String,
    ca_certificate: Vec<u8>,
    client_certificate: Vec<u8>,
    client_key: Vec<u8>,
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
    let relay = Arc::new(settings.relay);
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

    let (product_catalog, contract_snapshot) =
        cy_workspace_web_bff::load_product_catalog_and_snapshot_from_environment()
            .map_err(|_| HostStartupError::ProductCatalog)?;
    let snapshot_manager = Arc::new(ContractSnapshotManager::new(contract_snapshot));

    let outbox: Arc<dyn cy_workspace_control_plane::authority_service::AuthorityOutboxStore> =
        match PostgresWorkspaceOutbox::connect_from_environment().await {
            Ok(ob) => Arc::new(ob),
            Err(_) => Arc::new(cy_workspace_control_plane::InMemoryAuthorityOutbox::default()),
        };
    let authority_service = Arc::new(WorkspaceAuthorityServiceImpl::new(
        snapshot_manager,
        outbox,
        b"cyrene-platform-authority-secret-32".to_vec(),
    ));

    let bridge_uds = env::var("CYRENE_WORKSPACE_WEB_BFF_BRIDGE_UDS")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            let default_path = PathBuf::from("/tmp/cyrene-frontend-bridge.sock");
            if default_path.exists() {
                Some(default_path)
            } else {
                None
            }
        });
    let bridge_tcp = env::var("CYRENE_WORKSPACE_WEB_BFF_BRIDGE_TCP").ok();

    let resolver: Arc<dyn WorkspaceApiResolver> = Arc::new(PrincipalScopedBridgeResolver {
        bridge_uds_path: bridge_uds,
        bridge_tcp_endpoint: bridge_tcp,
        relay_endpoint: relay.endpoint.clone(),
        relay_server_name: relay.server_name.clone(),
        handoff_issuer,
        authority_service,
    });
    let workspace_directory: Arc<dyn WorkspaceDirectory> = directory.clone();
    let workspace_gateway = Arc::new(FabricWorkspaceProductGateway::new(
        Arc::clone(&workspace_directory),
        resolver,
    ));
    let state = Arc::new(
        WebBffState::new(
            web_config,
            principal_verifier,
            workspace_directory,
            workspace_gateway,
            product_catalog,
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
        relay,
        authorization_store,
        attempt_reservation,
        registry,
        ca,
        credential_store,
        session_bindings,
        product_catalog_loaded: true,
    });

    Ok(with_probes(application, readiness))
}

/// Start liveness and check each live trust dependency without inventing a user identity.
///
/// Readiness probes the current PostgreSQL schemas, signed CRL, pinned OIDC discovery/JWKS, and
/// the native mTLS connection to Relay. Product authorization remains request-scoped and is
/// checked only after a real verified browser principal arrives.
///
/// 启动 liveness 并逐项探测真实 PostgreSQL schema、签名 CRL、OIDC metadata 与 Relay native mTLS；不伪造用户身份。
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
    relay: Arc<RelaySettings>,
    authorization_store: Arc<PostgresDeviceAuthorizationStore>,
    attempt_reservation: Arc<PostgresUserCodeAttemptReservation>,
    registry: Arc<PostgresWorkspaceDeviceRegistry>,
    ca: Arc<PostgresRestrictedDeviceCa>,
    credential_store: Arc<PostgresWebAuthnCredentialStore>,
    session_bindings: Arc<PostgresWebAuthnHttpSessionBindingStore>,
    product_catalog_loaded: bool,
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
        let relay_mtls = timeout(
            READINESS_TIMEOUT,
            cy_mtls_channel_client::connect_mtls_channel(
                &self.relay.endpoint,
                &self.relay.server_name,
                &self.relay.ca_certificate,
                &self.relay.client_certificate,
                &self.relay.client_key,
                Some(READINESS_TIMEOUT),
            ),
        )
        .await
        .is_ok_and(|result| result.is_ok());
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
            "relayNativeMtls": relay_mtls,
            "deviceAuthorizationDatabase": authorization_database,
            "userCodeAttemptStore": user_code_attempt_store,
            "deviceCertificateRegistry": registry,
            "deviceCaSignedCrl": current_signed_crl,
            "webauthnCredentialStore": webauthn_credentials,
            "webauthnSessionBindingStore": webauthn_session_bindings,
            "productContractBundle": self.product_catalog_loaded,
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

struct PrincipalScopedBridgeResolver {
    bridge_uds_path: Option<PathBuf>,
    bridge_tcp_endpoint: Option<String>,
    relay_endpoint: String,
    relay_server_name: String,
    handoff_issuer: Arc<WebRelaySessionIssuer>,
    authority_service: Arc<WorkspaceAuthorityServiceImpl>,
}

impl PrincipalScopedBridgeResolver {
    async fn connect_bridge(
        &self,
    ) -> Result<WorkspaceFrontendBridgeServiceClient<Channel>, WorkspaceApiResolutionError> {
        #[cfg(unix)]
        if let Some(ref socket_path) = self.bridge_uds_path {
            let path = socket_path.clone();
            let channel = Endpoint::try_from("http://[::]:50051")
                .map_err(|_| WorkspaceApiResolutionError::NotConfigured)?
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let p = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(p).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::tokio::TokioIo::new(stream))
                    }
                }))
                .await
                .map_err(|_| WorkspaceApiResolutionError::Unavailable)?;
            return Ok(WorkspaceFrontendBridgeServiceClient::new(channel));
        }

        if let Some(ref endpoint) = self.bridge_tcp_endpoint {
            let channel = Endpoint::from_shared(endpoint.clone())
                .map_err(|_| WorkspaceApiResolutionError::NotConfigured)?
                .connect()
                .await
                .map_err(|_| WorkspaceApiResolutionError::Unavailable)?;
            return Ok(WorkspaceFrontendBridgeServiceClient::new(channel));
        }

        // Fallback default UDS location
        #[cfg(unix)]
        {
            let path = PathBuf::from("/tmp/cyrene-frontend-bridge.sock");
            let channel = Endpoint::try_from("http://[::]:50051")
                .map_err(|_| WorkspaceApiResolutionError::NotConfigured)?
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let p = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(p).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::tokio::TokioIo::new(stream))
                    }
                }))
                .await
                .map_err(|_| WorkspaceApiResolutionError::Unavailable)?;
            return Ok(WorkspaceFrontendBridgeServiceClient::new(channel));
        }

        #[allow(unreachable_code)]
        Err(WorkspaceApiResolutionError::NotConfigured)
    }
}

#[async_trait]
impl WorkspaceApiResolver for PrincipalScopedBridgeResolver {
    async fn resolve(
        &self,
        principal: &VerifiedWebPrincipal,
        descriptor: &WorkspaceConnectionDescriptor,
    ) -> Result<WorkspaceApiBinding, WorkspaceApiResolutionError> {
        let bridge_client = self.connect_bridge().await?;

        let mut candidates = descriptor.candidates.iter().filter(|candidate| {
            candidate.mode == cy_proto::core_v1::ConnectivityMode::Relay as i32
                && candidate.connection_uri == self.relay_endpoint
                && candidate.server_name == self.relay_server_name
        });
        let candidate = candidates
            .next()
            .ok_or(WorkspaceApiResolutionError::NotConfigured)?;
        if candidates.next().is_some() {
            return Err(WorkspaceApiResolutionError::InvalidBinding);
        }

        let now_unix_ms = unix_now_ms().unwrap_or(0).max(0) as u64;
        let session_token = match self.handoff_issuer.issue(principal, now_unix_ms) {
            Ok(token) => token,
            Err(_) => return Err(WorkspaceApiResolutionError::Unavailable),
        };

        let api: Arc<dyn WorkspaceApi> = Arc::new(PrincipalScopedBridgeApi {
            bridge_client: Mutex::new(bridge_client),
            authority_service: Arc::clone(&self.authority_service),
            identity: principal.identity().clone(),
            organization_id: principal.organization_id().to_owned(),
            workspace_id: descriptor.workspace_id.clone(),
            relay_endpoint: self.relay_endpoint.clone(),
            session_token,
            expires_at_unix_ms: principal.expires_at_unix_ms(),
        });
        WorkspaceApiBinding::for_candidate(principal, descriptor, candidate, api)
    }
}

struct PrincipalScopedBridgeApi {
    bridge_client: Mutex<WorkspaceFrontendBridgeServiceClient<Channel>>,
    authority_service: Arc<WorkspaceAuthorityServiceImpl>,
    identity: UserIdentityRef,
    organization_id: String,
    workspace_id: String,
    relay_endpoint: String,
    session_token: String,
    expires_at_unix_ms: i64,
}

#[async_trait]
impl WorkspaceApi for PrincipalScopedBridgeApi {
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
                Some(workspace_api_request::Request::ProductApiV2(_))
            );
        if !caller_matches {
            return workspace_error(request_id, 7, "WORKSPACE_CALLER_CONTEXT_REQUIRED");
        }
        if self.expires_at_unix_ms <= unix_now_ms().unwrap_or(i64::MAX) {
            return workspace_error(request_id, 16, "WORKSPACE_CALLER_EXPIRED");
        }

        let Some(workspace_api_request::Request::ProductApiV2(invocation)) = request.request else {
            return workspace_error(request_id, 3, "INVALID_REQUEST_BODY");
        };

        // 1. Platform Authority approves and enqueues the invocation
        let caller_token = format!("{}:{}", self.identity.issuer, self.identity.subject);
        let approve_req = ApproveAndEnqueueInvocationRequest {
            workspace_id: self.workspace_id.clone(),
            caller_token,
            invocation: Some(invocation),
        };
        let approved = match self
            .authority_service
            .approve_and_enqueue_invocation(tonic::Request::new(approve_req))
            .await
        {
            Ok(resp) => {
                let inner = resp.into_inner();
                if let Some(err) = inner.error {
                    return workspace_error(
                        request_id,
                        err.code,
                        Box::leak(err.message.into_boxed_str()),
                    );
                }
                match inner.approved_invocation {
                    Some(app) => app,
                    None => return workspace_error(request_id, 13, "WORKSPACE_APPROVAL_FAILED"),
                }
            }
            Err(status) => {
                return workspace_error(
                    request_id,
                    status.code() as i32,
                    Box::leak(status.message().to_string().into_boxed_str()),
                );
            }
        };

        // 2. Dispatches to decoupled Plugins Frontend Bridge over local RPC
        let invocation_id = approved.invocation_id.clone();
        let credential = approved.credential.clone();
        let bridge_req = BridgeExecuteInvocationRequest {
            approved_invocation: Some(approved),
            relay_endpoint: self.relay_endpoint.clone(),
            session_token: self.session_token.clone(),
        };

        let bridge_call = self
            .bridge_client
            .lock()
            .await
            .execute_invocation(tonic::Request::new(bridge_req))
            .await;

        match bridge_call {
            Ok(resp) => {
                let inner = resp.into_inner();
                if inner.success && inner.product_response.is_some() {
                    let product_resp = inner.product_response.unwrap();
                    let _ = self
                        .authority_service
                        .submit_invocation_result(tonic::Request::new(
                            SubmitInvocationResultRequest {
                                invocation_id: invocation_id.clone(),
                                credential: credential.clone(),
                                outcome_status: ExecutionOutcomeStatus::Success as i32,
                                product_response: Some(product_resp.clone()),
                                error_message: String::new(),
                            },
                        ))
                        .await;
                    WorkspaceApiResponse {
                        request_id,
                        outcome: Some(workspace_api_response::Outcome::ProductApiV2(product_resp)),
                    }
                } else {
                    let _ = self
                        .authority_service
                        .submit_invocation_result(tonic::Request::new(
                            SubmitInvocationResultRequest {
                                invocation_id: invocation_id.clone(),
                                credential: credential.clone(),
                                outcome_status: ExecutionOutcomeStatus::Failed as i32,
                                product_response: None,
                                error_message: inner.error_message.clone(),
                            },
                        ))
                        .await;
                    workspace_error(request_id, 13, "WORKSPACE_BRIDGE_EXECUTION_FAILED")
                }
            }
            Err(status) => {
                let _ = self
                    .authority_service
                    .submit_invocation_result(tonic::Request::new(
                        SubmitInvocationResultRequest {
                            invocation_id: invocation_id.clone(),
                            credential: credential.clone(),
                            outcome_status: ExecutionOutcomeStatus::UnknownResult as i32,
                            product_response: None,
                            error_message: format!("Transport error: {}", status.message()),
                        },
                    ))
                    .await;
                workspace_error(request_id, 14, "WORKSPACE_BRIDGE_UNAVAILABLE")
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
