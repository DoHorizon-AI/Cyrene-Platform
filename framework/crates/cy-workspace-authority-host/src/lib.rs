//! Production composition and protected local administration for Workspace Authority.

#![forbid(unsafe_code)]

use std::{
    env,
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Read},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener as StdUnixListener,
    },
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use axum::{routing::get, Router};
use cy_proto::cyrene::workspace::authority::v2::workspace_authority_service_server::WorkspaceAuthorityServiceServer;
use cy_workspace_control_plane::{
    authority::{
        AuthorityCredentialSigner, AuthorityExecutionTargetStore, AuthorityRpcService,
        AuthorityServiceDependencies, AuthorityWebSessionStore, ContractSnapshotManager,
        SnapshotArtifactInput, SnapshotTrust, VerifiedContractSnapshot,
    },
    AzureAdWebIdentityConfig, AzureAdWebPrincipalVerifier, WebIdentityDirectory,
    WebIdentityDirectoryError,
};
use cy_workspace_postgres_storage::{
    AuthorityExecutionTargetBinding as PgTarget, DurableDirectoryError,
    PostgresAuthorityExecutionTargetStore, PostgresAuthorityWebSessionStore,
    PostgresWorkspaceDeviceRegistry, PostgresWorkspaceDirectory, PostgresWorkspaceOutbox,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use tokio::{
    net::{TcpListener, UnixListener},
    sync::RwLock,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tower::{Layer, Service};
use tracing::{error, info};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const STATE_DIR_DEFAULT: &str = "/var/lib/cyrene-workspace-authority";
const BUNDLE_ROOT_DEFAULT: &str = "/var/lib/cyrene-product-bundles";
const BFF_UDS_DEFAULT: &str = "/run/cyrene-workspace-authority/bff/bff.sock";
const ADMIN_UDS_DEFAULT: &str = "/run/cyrene-workspace-authority/admin.sock";
const MAX_ADMIN_LINE: usize = 32 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("Authority configuration is invalid or incomplete")]
    Configuration,
    #[error("Authority protected file is missing or unsafe")]
    ProtectedFile,
    #[error("Authority database dependencies are unavailable")]
    Database,
    #[error("Authority trust or active artifact validation failed")]
    Artifact,
    #[error("Authority signer configuration is invalid")]
    Signer,
    #[error("Authority mTLS configuration is invalid")]
    Tls,
    #[error("Authority listener setup failed")]
    Listener(#[from] io::Error),
}

#[derive(Clone)]
pub struct AdminState {
    trust: Arc<SnapshotTrust>,
    manager: Arc<ContractSnapshotManager>,
    bundle_root: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminRequest {
    action: String,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    plan_digest: Option<String>,
    #[serde(default)]
    expected_generation: Option<u64>,
    #[serde(default)]
    artifact_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminResponse {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    highest_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<cy_workspace_control_plane::authority::SnapshotIdentity>,
}

impl AdminResponse {
    fn error(code: &'static str, message: &'static str) -> Self {
        Self {
            status: "error".into(),
            code: Some(code.into()),
            message: Some(message.into()),
            plan_id: None,
            plan_digest: None,
            artifact_id: None,
            current_generation: None,
            highest_generation: None,
            next_generation: None,
            activation_epoch: None,
            active_artifact_id: None,
            identity: None,
        }
    }
}

pub async fn run_host() -> Result<(), HostError> {
    let mut settings = Settings::load()?;
    if settings.state_dir != Path::new(STATE_DIR_DEFAULT)
        || settings.bff_uds != Path::new(BFF_UDS_DEFAULT)
        || settings.admin_uds != Path::new(ADMIN_UDS_DEFAULT)
        || settings.bff_uds == settings.admin_uds
        || settings
            .listen
            .parse::<std::net::SocketAddr>()
            .map_err(|_| HostError::Configuration)?
            .port()
            != 50051
        || settings
            .health_addr
            .parse::<std::net::SocketAddr>()
            .map_err(|_| HostError::Configuration)?
            .port()
            != 8080
    {
        return Err(HostError::Configuration);
    }
    let trust =
        Arc::new(SnapshotTrust::new(&settings.trust_config).map_err(|_| HostError::Artifact)?);
    trust
        .validate_activation_state_dir(&settings.state_dir)
        .map_err(|_| HostError::Artifact)?;
    let expected_versions =
        fs::canonicalize(settings.bundle_root.join("versions")).map_err(|_| HostError::Artifact)?;
    let configured_versions =
        fs::canonicalize(trust.artifact_versions_root()).map_err(|_| HostError::Artifact)?;
    if expected_versions != configured_versions {
        return Err(HostError::Artifact);
    }
    let selected = read_initial_artifact(&settings, &trust)?;
    let directory = Arc::new(
        PostgresWorkspaceDirectory::connect(&settings.database_url)
            .await
            .map_err(|_| HostError::Database)?,
    );
    directory
        .health_check()
        .await
        .map_err(|_| HostError::Database)?;
    let identity_directory: Arc<dyn WebIdentityDirectory> = Arc::new(IdentityDirectory {
        directory: directory.clone(),
    });
    let verifier = Arc::new(
        AzureAdWebPrincipalVerifier::new(
            AzureAdWebIdentityConfig::new(settings.tenant_id, settings.audience.clone())
                .map_err(|_| HostError::Configuration)?,
            identity_directory,
        )
        .map_err(|_| HostError::Configuration)?,
    );
    verifier
        .check_provider_readiness()
        .await
        .map_err(|_| HostError::Configuration)?;

    let pg_options = PgConnectOptions::from_str(&settings.database_url)
        .map_err(|_| HostError::Configuration)?
        .ssl_mode(PgSslMode::VerifyFull)
        .application_name("cyrene-workspace-authority")
        .options([("statement_timeout", "10000"), ("lock_timeout", "5000")]);
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(pg_options)
        .await
        .map_err(|_| HostError::Database)?;
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|_| HostError::Database)?;
    verify_authority_schema(&pool).await?;
    let readiness_pool = pool.clone();
    let sessions: Arc<dyn AuthorityWebSessionStore> = Arc::new(SessionAdapter {
        store: PostgresAuthorityWebSessionStore::new(pool.clone()),
    });
    let targets: Arc<dyn AuthorityExecutionTargetStore> = Arc::new(TargetAdapter {
        store: PostgresAuthorityExecutionTargetStore::new(pool.clone()),
    });
    let outbox = Arc::new(PostgresWorkspaceOutbox::new(pool));
    let registry = Arc::new(
        tokio::task::spawn_blocking({
            let url = settings.database_url.clone();
            move || PostgresWorkspaceDeviceRegistry::connect(&url)
        })
        .await
        .map_err(|_| HostError::Database)?
        .map_err(|_| HostError::Database)?,
    );
    registry.health_check().map_err(|_| HostError::Database)?;

    let signing_key = SigningKey::from_bytes(&settings.signing_key);
    settings.signing_key.zeroize();
    let signer = AuthorityCredentialSigner::new(
        settings.signing_key_id.clone(),
        signing_key,
        Duration::from_secs(180),
    )
    .map_err(|_| HostError::Signer)?;
    let initial = verified_snapshot(
        &trust,
        &settings.bundle_root,
        &selected.artifact_id,
        selected.generation,
        selected.epoch,
    )?;
    let manager = Arc::new(
        ContractSnapshotManager::open(&settings.state_dir, initial)
            .map_err(|_| HostError::Artifact)?,
    );
    let state = Arc::new(AdminState {
        bundle_root: settings.bundle_root.clone(),
        trust,
        manager,
    });

    let rpc = AuthorityRpcService::new(AuthorityServiceDependencies {
        snapshots: state.manager.clone(),
        web_verifier: verifier.clone(),
        directory: directory.clone(),
        sessions,
        targets,
        device_registry: registry.clone(),
        outbox,
        signer,
    });

    prepare_private_socket_parent(&settings.bff_uds)?;
    prepare_private_socket_parent(&settings.admin_uds)?;
    let bff_listener = bind_private_uds(&settings.bff_uds)?;
    let admin_listener = bind_private_uds(&settings.admin_uds)?;
    let tcp = TcpListener::bind(&settings.listen).await?;
    let health = TcpListener::bind(&settings.health_addr).await?;

    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&settings.tls_cert, &settings.tls_key))
        .client_ca_root(Certificate::from_pem(&settings.client_ca));
    let tcp_server = Server::builder()
        .tls_config(tls)
        .map_err(|_| HostError::Tls)?;
    let ready = Arc::new(RwLock::new(false));
    *ready.write().await = true;
    let health_router = Router::new().route("/livez", get(|| async { "ok" })).route(
        "/readyz",
        get({
            let ready = ready.clone();
            move || {
                let ready = ready.clone();
                async move {
                    if *ready.read().await {
                        (http::StatusCode::OK, "ready")
                    } else {
                        (http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
                    }
                }
            }
        }),
    );
    let readiness = ready.clone();
    let readiness_directory = directory.clone();
    let readiness_registry = registry.clone();
    let readiness_verifier = verifier.clone();
    let readiness_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;
        loop {
            interval.tick().await;
            let directory_ok = readiness_directory.health_check().await.is_ok();
            let db_ok = verify_authority_schema(&readiness_pool).await.is_ok();
            let verifier_ok = readiness_verifier.check_provider_readiness().await.is_ok();
            let registry_for_check = readiness_registry.clone();
            let registry_ok =
                tokio::task::spawn_blocking(move || registry_for_check.health_check())
                    .await
                    .is_ok_and(|result| result.is_ok());
            *readiness.write().await = directory_ok && db_ok && verifier_ok && registry_ok;
        }
    });
    let health_task = tokio::spawn(async move { axum::serve(health, health_router).await });
    let rpc_bff = rpc.clone();
    let bff_peer_uid = settings.bff_peer_uid;
    let admin_peer_uid = settings.admin_peer_uid;
    let bff_task = tokio::spawn(async move {
        Server::builder()
            .layer(BffRpcPathLayer)
            .add_service(WorkspaceAuthorityServiceServer::with_interceptor(
                rpc_bff,
                bff_peer_interceptor(bff_peer_uid),
            ))
            .serve_with_incoming(UnixListenerStream::new(bff_listener))
            .await
    });
    let admin_task = tokio::spawn(serve_admin(admin_listener, state, admin_peer_uid));
    let tcp_task = tokio::spawn(async move {
        tcp_server
            .layer(TcpRpcPathLayer)
            .add_service(WorkspaceAuthorityServiceServer::new(rpc))
            .serve_with_incoming(TcpListenerStream::new(tcp))
            .await
    });
    drop(settings);
    info!("Workspace Authority listeners are ready");
    tokio::select! {
        result = tcp_task => report_task("mTLS RPC", result),
        result = bff_task => report_task("BFF UDS RPC", result),
        result = admin_task => report_task("admin UDS", result),
        result = health_task => report_task("health HTTP", result),
        result = readiness_task => { error!(?result, "Authority readiness monitor stopped"); Err(HostError::Configuration) },
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

fn report_task(
    name: &str,
    result: Result<Result<(), impl std::fmt::Display>, tokio::task::JoinError>,
) -> Result<(), HostError> {
    match result {
        Ok(Ok(())) => Err(HostError::Configuration),
        Ok(Err(error)) => {
            error!(listener = name, %error, "Authority listener stopped");
            Err(HostError::Configuration)
        }
        Err(_) => Err(HostError::Configuration),
    }
}

async fn verify_authority_schema(pool: &sqlx::PgPool) -> Result<(), HostError> {
    const REQUIRED_QUERIES: [&str; 3] = [
        "SELECT invocation_id, workspace_id, status, execution_credential_proto, approved_envelope_proto, result_digest_sha256 FROM cyrene_workspace_device_registry.workspace_invocation_outbox LIMIT 0",
        "SELECT session_id, organization_id, workspace_id, principal_issuer, principal_subject, bearer_token_sha256, session_generation, expires_at_unix_ms FROM cyrene_workspace_device_registry.authority_web_sessions LIMIT 0",
        "SELECT organization_id, workspace_id, operation_owner_id, operation_id, execution_device_id, execution_authorization_id, certificate_fingerprint_sha256, endpoint_base, contract_activation_generation FROM cyrene_workspace_device_registry.authority_execution_target_bindings LIMIT 0",
    ];
    for query in REQUIRED_QUERIES {
        sqlx::query(query)
            .execute(pool)
            .await
            .map_err(|_| HostError::Database)?;
    }
    Ok(())
}

struct Settings {
    listen: String,
    health_addr: String,
    state_dir: PathBuf,
    bundle_root: PathBuf,
    trust_config: PathBuf,
    bff_uds: PathBuf,
    admin_uds: PathBuf,
    initial_artifact_id_file: PathBuf,
    database_url: Zeroizing<String>,
    tenant_id: Uuid,
    audience: String,
    signing_key: [u8; 32],
    signing_key_id: String,
    tls_cert: Vec<u8>,
    tls_key: Vec<u8>,
    client_ca: Vec<u8>,
    bff_peer_uid: u32,
    admin_peer_uid: u32,
}

impl Settings {
    fn load() -> Result<Self, HostError> {
        let database_url_file = path_env("CYRENE_AUTHORITY_DATABASE_URL_FILE")?;
        let mut database_url_bytes = Zeroizing::new(read_secret_file(&database_url_file, 8192)?);
        while matches!(database_url_bytes.last(), Some(b'\n' | b'\r')) {
            database_url_bytes.pop();
        }
        let database_url = Zeroizing::new(
            String::from_utf8(database_url_bytes.to_vec()).map_err(|_| HostError::Configuration)?,
        );
        if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
            return Err(HostError::Configuration);
        }
        let signing_key_file = path_env("CYRENE_AUTHORITY_SIGNING_KEY_FILE")?;
        let mut signing_key_vec = read_secret_file(&signing_key_file, 32)?;
        let signing_key: [u8; 32] = signing_key_vec
            .as_slice()
            .try_into()
            .map_err(|_| HostError::Signer)?;
        signing_key_vec.zeroize();
        let tls_cert = read_secret_file(&path_env("CYRENE_AUTHORITY_TLS_CERT_FILE")?, 1024 * 1024)?;
        let tls_key = read_secret_file(&path_env("CYRENE_AUTHORITY_TLS_KEY_FILE")?, 1024 * 1024)?;
        let client_ca = read_secret_file(
            &path_env("CYRENE_AUTHORITY_TLS_CLIENT_CA_FILE")?,
            1024 * 1024,
        )?;
        Ok(Self {
            listen: env_value("CYRENE_WORKSPACE_AUTHORITY_LISTEN", "0.0.0.0:50051")?,
            health_addr: loopback_addr(env_value(
                "CYRENE_WORKSPACE_AUTHORITY_HEALTH_ADDR",
                "127.0.0.1:8080",
            )?)?,
            state_dir: path_value("CYRENE_WORKSPACE_AUTHORITY_STATE_DIR", STATE_DIR_DEFAULT)?,
            bundle_root: PathBuf::from(BUNDLE_ROOT_DEFAULT),
            trust_config: path_env("CYRENE_WORKSPACE_AUTHORITY_TRUST_CONFIG")?,
            bff_uds: path_value("CYRENE_WORKSPACE_AUTHORITY_BFF_UDS", BFF_UDS_DEFAULT)?,
            admin_uds: path_value("CYRENE_WORKSPACE_AUTHORITY_ADMIN_UDS", ADMIN_UDS_DEFAULT)?,
            initial_artifact_id_file: PathBuf::from(
                env::var("CYRENE_AUTHORITY_INITIAL_ARTIFACT_ID_FILE").unwrap_or_else(|_| {
                    "/etc/cyrene-workspace-authority/initial-artifact-id".into()
                }),
            ),
            database_url,
            tenant_id: env::var("CYRENE_AUTHORITY_AAD_TENANT_ID")
                .map_err(|_| HostError::Configuration)?
                .parse()
                .map_err(|_| HostError::Configuration)?,
            audience: env_value("CYRENE_AUTHORITY_AAD_AUDIENCE", "")?,
            signing_key,
            signing_key_id: env_value("CYRENE_AUTHORITY_SIGNING_KEY_ID", "")?,
            tls_cert,
            tls_key,
            client_ca,
            bff_peer_uid: env::var("CYRENE_AUTHORITY_BFF_PEER_UID")
                .unwrap_or_else(|_| "10001".into())
                .parse()
                .map_err(|_| HostError::Configuration)?,
            admin_peer_uid: env::var("CYRENE_AUTHORITY_ADMIN_PEER_UID")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .map_err(|_| HostError::Configuration)?,
        })
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        self.signing_key.zeroize();
        self.tls_key.zeroize();
    }
}

struct SelectedArtifact {
    artifact_id: String,
    generation: u64,
    epoch: u64,
}

fn read_initial_artifact(
    settings: &Settings,
    trust: &SnapshotTrust,
) -> Result<SelectedArtifact, HostError> {
    if let Some(record) = ContractSnapshotManager::read_activation_record(&settings.state_dir)
        .map_err(|_| HostError::Artifact)?
    {
        return Ok(SelectedArtifact {
            artifact_id: record.artifact_id,
            generation: record.current_generation,
            epoch: record.activation_epoch,
        });
    }
    let bytes = read_protected_file(&settings.initial_artifact_id_file, 128)?;
    let artifact_id = std::str::from_utf8(&bytes)
        .map_err(|_| HostError::Configuration)?
        .trim()
        .to_owned();
    if !is_artifact_id(&artifact_id) {
        return Err(HostError::Configuration);
    }
    let _ = trust
        .resolve_artifact_root(&artifact_id)
        .map_err(|_| HostError::Artifact)?;
    Ok(SelectedArtifact {
        artifact_id,
        generation: 1,
        epoch: 1,
    })
}

fn verified_snapshot(
    trust: &SnapshotTrust,
    bundle_root: &Path,
    artifact_id: &str,
    generation: u64,
    epoch: u64,
) -> Result<VerifiedContractSnapshot, HostError> {
    let raw = artifact_hex(artifact_id).ok_or(HostError::Artifact)?;
    let meta_dir = bundle_root.join("metadata").join(raw);
    validate_private_directory(&meta_dir)?;
    let manifest_path = meta_dir.join("component-manifest-v2.json");
    let manifest = read_protected_file(&manifest_path, 2 * 1024 * 1024)?;
    let value: Value = serde_json::from_slice(&manifest).map_err(|_| HostError::Artifact)?;
    verify_outer_manifest(&value, artifact_id)?;
    verify_import_record(&meta_dir, &value, artifact_id)?;
    let proof_sha = value
        .pointer("/dataBundle/proofSha256")
        .and_then(Value::as_str)
        .ok_or(HostError::Artifact)?;
    trust
        .verify_and_load(SnapshotArtifactInput {
            artifact_id,
            archive_path: &trust
                .resolve_archive_path(artifact_id)
                .map_err(|_| HostError::Artifact)?,
            artifact_root: &trust
                .resolve_artifact_root(artifact_id)
                .map_err(|_| HostError::Artifact)?,
            proof_path: "data-bundle-proof-v1.json",
            proof_sha256: proof_sha,
            generation,
            activation_epoch: epoch,
        })
        .map_err(|_| HostError::Artifact)
}

fn verify_outer_manifest(manifest: &Value, artifact_id: &str) -> Result<(), HostError> {
    let object = manifest.as_object().ok_or(HostError::Artifact)?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(2) {
        return Err(HostError::Artifact);
    }
    let expected = object
        .get("manifestDigest")
        .and_then(Value::as_str)
        .ok_or(HostError::Artifact)?;
    if !is_artifact_id(expected) {
        return Err(HostError::Artifact);
    }
    let mut unsigned = manifest.clone();
    unsigned
        .as_object_mut()
        .ok_or(HostError::Artifact)?
        .remove("manifestDigest");
    let canonical = serde_jcs::to_vec(&unsigned).map_err(|_| HostError::Artifact)?;
    if format!("sha256:{}", hex(&Sha256::digest(canonical))) != expected {
        return Err(HostError::Artifact);
    }
    if manifest.pointer("/artifact/kind").and_then(Value::as_str) != Some("data-bundle")
        || manifest.pointer("/artifact/sha256").and_then(Value::as_str) != Some(artifact_id)
        || object.get("contentDigest").and_then(Value::as_str) != Some(artifact_id)
        || manifest
            .pointer("/dataBundle/proofPath")
            .and_then(Value::as_str)
            != Some("data-bundle-proof-v1.json")
        || !manifest
            .pointer("/dataBundle/proofSha256")
            .and_then(Value::as_str)
            .is_some_and(is_artifact_id)
    {
        return Err(HostError::Artifact);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanReference {
    schema_version: u32,
    plan_id: String,
    plan_digest: String,
    artifact_id: String,
    manifest_digest: String,
    proof_sha256: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    origin_plan_id: Option<String>,
    #[serde(default)]
    origin_plan_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportRecord {
    schema_version: u32,
    artifact_id: String,
    component_id: String,
    channel: String,
    manifest_digest: String,
    manifest_uri: String,
    index_uri: String,
    index_digest: String,
    index_source: String,
    publisher_repository: String,
}

fn verify_plan_reference(
    state: &AdminState,
    request: &AdminRequest,
) -> Result<(String, String, String), HostError> {
    let plan_id = request.plan_id.as_deref().ok_or(HostError::Configuration)?;
    if plan_id.is_empty()
        || plan_id.len() > 128
        || !plan_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(HostError::Configuration);
    }
    let artifact_id = request
        .artifact_id
        .as_deref()
        .ok_or(HostError::Configuration)?;
    let raw = artifact_hex(artifact_id).ok_or(HostError::Configuration)?;
    let metadata_root = state.bundle_root.join("metadata");
    validate_private_directory(&metadata_root)?;
    let meta_dir = metadata_root.join(raw);
    validate_private_directory(&meta_dir)?;
    let plans_dir = meta_dir.join("plans");
    validate_private_directory(&plans_dir)?;
    let path = plans_dir.join(format!("{plan_id}.json"));
    let bytes = read_protected_file(&path, 128 * 1024)?;
    let plan_value: Value = serde_json::from_slice(&bytes).map_err(|_| HostError::Artifact)?;
    let plan_object = plan_value.as_object().ok_or(HostError::Artifact)?;
    let rollback_fields =
        ["action", "originPlanId", "originPlanDigest"].map(|field| plan_object.contains_key(field));
    let plan: PlanReference =
        serde_json::from_value(plan_value).map_err(|_| HostError::Artifact)?;
    if plan.schema_version != 1 || plan.plan_id != plan_id {
        return Err(HostError::Artifact);
    }
    if request.plan_digest.as_deref() != Some(plan.plan_digest.as_str())
        || artifact_id != plan.artifact_id
        || !is_artifact_id(&plan.plan_digest)
        || !is_artifact_id(&plan.manifest_digest)
        || !is_artifact_id(&plan.proof_sha256)
    {
        return Err(HostError::Artifact);
    }
    match (
        &plan.action,
        &plan.origin_plan_id,
        &plan.origin_plan_digest,
        rollback_fields,
    ) {
        (None, None, None, [false, false, false]) => {}
        (Some(action), Some(origin_id), Some(origin_digest), [true, true, true])
            if action == "rollback"
                && valid_plan_id(origin_id)
                && is_artifact_id(origin_digest) =>
        {
            let origin_path = plans_dir.join(format!("{origin_id}.json"));
            let origin_value: Value =
                serde_json::from_slice(&read_protected_file(&origin_path, 128 * 1024)?)
                    .map_err(|_| HostError::Artifact)?;
            let origin_object = origin_value.as_object().ok_or(HostError::Artifact)?;
            let origin_has_action = origin_object.contains_key("action");
            let origin_has_plan_id = origin_object.contains_key("originPlanId");
            let origin_has_digest = origin_object.contains_key("originPlanDigest");
            let origin: PlanReference =
                serde_json::from_value(origin_value).map_err(|_| HostError::Artifact)?;
            if origin.action.is_some()
                || origin_has_action
                || origin_has_plan_id
                || origin_has_digest
                || origin.origin_plan_id.is_some()
                || origin.origin_plan_digest.is_some()
                || origin.plan_id != origin_id.as_str()
                || origin.plan_digest != origin_digest.as_str()
                || origin.artifact_id != plan.artifact_id
                || origin.manifest_digest != plan.manifest_digest
                || origin.proof_sha256 != plan.proof_sha256
            {
                return Err(HostError::Artifact);
            }
        }
        _ => return Err(HostError::Artifact),
    }
    let manifest_path = meta_dir.join("component-manifest-v2.json");
    let manifest: Value =
        serde_json::from_slice(&read_protected_file(&manifest_path, 2 * 1024 * 1024)?)
            .map_err(|_| HostError::Artifact)?;
    verify_outer_manifest(&manifest, &plan.artifact_id)?;
    let digest = manifest
        .get("manifestDigest")
        .and_then(Value::as_str)
        .ok_or(HostError::Artifact)?;
    let proof = manifest
        .pointer("/dataBundle/proofSha256")
        .and_then(Value::as_str)
        .ok_or(HostError::Artifact)?;
    if digest != plan.manifest_digest || proof != plan.proof_sha256 {
        return Err(HostError::Artifact);
    }
    verify_import_record(&meta_dir, &manifest, &plan.artifact_id)?;
    Ok((plan.plan_id, plan.plan_digest, plan.artifact_id))
}

fn valid_plan_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn verify_import_record(
    meta_dir: &Path,
    manifest: &Value,
    artifact_id: &str,
) -> Result<(), HostError> {
    let import_path = meta_dir.join("import-record-v1.json");
    let import: ImportRecord =
        serde_json::from_slice(&read_protected_file(&import_path, 64 * 1024)?)
            .map_err(|_| HostError::Artifact)?;
    let manifest_digest = manifest
        .get("manifestDigest")
        .and_then(Value::as_str)
        .ok_or(HostError::Artifact)?;
    if import.schema_version != 1
        || import.artifact_id != artifact_id
        || import.manifest_digest != manifest_digest
        || import.component_id
            != manifest
                .get("componentId")
                .and_then(Value::as_str)
                .unwrap_or_default()
        || import.channel.trim().is_empty()
        || import.manifest_uri.trim().is_empty()
        || import.index_uri.trim().is_empty()
        || !is_artifact_id(&import.index_digest)
        || import.index_source.trim().is_empty()
        || import.publisher_repository.trim().is_empty()
    {
        return Err(HostError::Artifact);
    }
    Ok(())
}

async fn serve_admin(
    listener: UnixListener,
    state: Arc<AdminState>,
    peer_uid: u32,
) -> Result<(), io::Error> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let result = async {
                let credentials = stream.peer_cred()?;
                if credentials.uid() != peer_uid {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "admin peer denied",
                    ));
                }
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                let mut newline_received = false;
                while line.len() <= MAX_ADMIN_LINE {
                    if tokio::io::AsyncReadExt::read(&mut stream, &mut byte).await? == 0 {
                        break;
                    }
                    if byte[0] == b'\n' {
                        newline_received = true;
                        break;
                    }
                    line.push(byte[0]);
                }
                if line.len() > MAX_ADMIN_LINE || !newline_received {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "request too large",
                    ));
                }
                let request: AdminRequest = serde_json::from_slice(&line)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid request"))?;
                let response = tokio::task::spawn_blocking(move || handle_admin(&state, request))
                    .await
                    .unwrap_or_else(|_| {
                        AdminResponse::error("INTERNAL", "Authority admin operation failed")
                    });
                let mut bytes = serde_json::to_vec(&response)
                    .map_err(|_| io::Error::other("serialize response"))?;
                bytes.push(b'\n');
                tokio::io::AsyncWriteExt::write_all(&mut stream, &bytes).await?;
                Ok::<_, io::Error>(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "rejected Authority admin request");
            }
        });
    }
}

fn handle_admin(state: &AdminState, request: AdminRequest) -> AdminResponse {
    let current = match state.manager.active_snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => {
            return AdminResponse::error(
                "STATE_UNAVAILABLE",
                "Authority activation state is unavailable",
            )
        }
    };
    let high = state.manager.highest_generation();
    let Some(record) = ContractSnapshotManager::read_activation_record(state.manager.state_dir())
        .ok()
        .flatten()
    else {
        return AdminResponse::error(
            "STATE_UNAVAILABLE",
            "Authority activation state is unavailable",
        );
    };
    if request.action == "status" {
        if request.plan_id.is_some()
            || request.plan_digest.is_some()
            || request.artifact_id.is_some()
            || request.expected_generation.is_some()
        {
            return AdminResponse::error("INVALID_REQUEST", "Authority admin request is invalid");
        }
        return AdminResponse {
            status: "ok".into(),
            code: None,
            message: None,
            plan_id: None,
            plan_digest: None,
            artifact_id: None,
            current_generation: Some(record.current_generation),
            highest_generation: Some(record.highest_generation),
            next_generation: None,
            activation_epoch: Some(record.activation_epoch),
            active_artifact_id: Some(record.artifact_id.clone()),
            identity: Some(current.identity.clone()),
        };
    }
    if !matches!(
        request.action.as_str(),
        "validateArtifact" | "activateArtifact"
    ) || (request.action == "validateArtifact" && request.expected_generation.is_some())
        || (request.action == "activateArtifact" && request.expected_generation.is_none())
    {
        return AdminResponse::error("INVALID_REQUEST", "Authority admin request is invalid");
    }
    if request.action == "activateArtifact" && request.expected_generation != Some(high) {
        return AdminResponse::error(
            "GENERATION_CONFLICT",
            "Expected Authority generation is stale",
        );
    }
    let (plan_id, plan_digest, artifact_id) = match verify_plan_reference(state, &request) {
        Ok(value) => value,
        Err(_) => {
            return AdminResponse::error("ARTIFACT_INVALID", "Selected artifact or plan is invalid")
        }
    };
    let Some(next_generation) = high.checked_add(1) else {
        return AdminResponse::error(
            "GENERATION_EXHAUSTED",
            "Authority generation space is exhausted",
        );
    };
    let verified = match verified_snapshot(
        &state.trust,
        &state.bundle_root,
        &artifact_id,
        next_generation,
        next_generation,
    ) {
        Ok(value) => value,
        Err(_) => {
            return AdminResponse::error(
                "ARTIFACT_INVALID",
                "Selected artifact failed trust validation",
            )
        }
    };
    if request.action == "validateArtifact" {
        return AdminResponse {
            status: "valid".into(),
            code: None,
            message: None,
            plan_id: Some(plan_id),
            plan_digest: Some(plan_digest),
            artifact_id: Some(artifact_id),
            current_generation: Some(record.current_generation),
            highest_generation: Some(high),
            next_generation: Some(next_generation),
            activation_epoch: Some(next_generation),
            active_artifact_id: None,
            identity: Some(verified.identity().clone()),
        };
    }
    if request.expected_generation != Some(high) {
        return AdminResponse::error(
            "GENERATION_CONFLICT",
            "Expected Authority generation is stale",
        );
    }
    match state
        .manager
        .activate_verified(verified, &plan_id, &plan_digest, high)
    {
        Ok(record) => AdminResponse {
            status: "activated".into(),
            code: None,
            message: None,
            plan_id: Some(plan_id),
            plan_digest: Some(plan_digest),
            artifact_id: Some(artifact_id),
            current_generation: Some(record.current_generation),
            highest_generation: Some(record.highest_generation),
            next_generation: None,
            activation_epoch: Some(record.activation_epoch),
            active_artifact_id: Some(record.artifact_id),
            identity: Some(record.identity),
        },
        Err(_) => AdminResponse::error(
            "ACTIVATION_REJECTED",
            "Authority artifact activation was rejected",
        ),
    }
}

#[derive(Clone)]
struct IdentityDirectory {
    directory: Arc<PostgresWorkspaceDirectory>,
}
#[async_trait]
impl WebIdentityDirectory for IdentityDirectory {
    async fn organizations_for_verified_identity(
        &self,
        identity: &cy_proto::workspace_v1::UserIdentityRef,
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

struct SessionAdapter {
    store: PostgresAuthorityWebSessionStore,
}
#[async_trait]
impl AuthorityWebSessionStore for SessionAdapter {
    async fn resolve_verified_bearer(
        &self,
        principal: &cy_workspace_control_plane::VerifiedWebPrincipal,
        workspace_id: &str,
        bearer_token: &str,
        now_unix_ms: u64,
    ) -> Result<cy_workspace_control_plane::authority::AuthorityWebSession, tonic::Status> {
        let session = self
            .store
            .resolve_verified_bearer(principal, workspace_id, bearer_token, now_unix_ms)
            .await
            .map_err(|_| tonic::Status::unavailable("web session store unavailable"))?;
        Ok(cy_workspace_control_plane::authority::AuthorityWebSession {
            session_id: session.session_id,
            organization_id: session.organization_id,
            workspace_id: session.workspace_id,
            principal_issuer: session.principal_issuer,
            principal_subject: session.principal_subject,
            session_generation: session.session_generation,
            issued_at_unix_ms: session.issued_at_unix_ms,
            expires_at_unix_ms: session.expires_at_unix_ms,
        })
    }
}

struct TargetAdapter {
    store: PostgresAuthorityExecutionTargetStore,
}
#[async_trait]
impl AuthorityExecutionTargetStore for TargetAdapter {
    async fn resolve(
        &self,
        organization_id: &str,
        workspace_id: &str,
        owner_id: &str,
        operation_id: &str,
    ) -> Result<Option<cy_workspace_control_plane::authority::AuthorityTargetBinding>, tonic::Status>
    {
        let Some(target) = self
            .store
            .resolve(organization_id, workspace_id, owner_id, operation_id)
            .await
            .map_err(|_| tonic::Status::unavailable("target store unavailable"))?
        else {
            return Ok(None);
        };
        Ok(Some(map_target(target)))
    }
}

fn map_target(target: PgTarget) -> cy_workspace_control_plane::authority::AuthorityTargetBinding {
    cy_workspace_control_plane::authority::AuthorityTargetBinding {
        target_component: target.target_component,
        execution_device_id: target.execution_device_id,
        execution_device_generation: target.execution_device_generation,
        execution_authorization_id: target.execution_authorization_id,
        certificate_fingerprint_sha256: target.certificate_fingerprint_sha256,
        target_binding_manifest_sha256: target.target_binding_manifest_sha256,
        bundle_manifest_sha256: target.bundle_manifest_sha256,
        contract_activation_generation: target.contract_activation_generation,
        owner_source_commit: target.owner_source_commit,
        endpoint_base: target.endpoint_base,
    }
}

#[allow(clippy::result_large_err)]
fn bff_peer_interceptor(
    expected_uid: u32,
) -> impl Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone {
    move |request| {
        let peer = request
            .extensions()
            .get::<tonic::transport::server::UdsConnectInfo>()
            .ok_or_else(|| tonic::Status::permission_denied("local BFF socket required"))?;
        if peer.peer_cred.as_ref().map(|credentials| credentials.uid()) != Some(expected_uid) {
            return Err(tonic::Status::permission_denied(
                "BFF peer is not authorized",
            ));
        }
        Ok(request)
    }
}

#[derive(Clone, Copy)]
struct TcpRpcPathLayer;

impl<S> Layer<S> for TcpRpcPathLayer {
    type Service = TcpRpcPathGate<S>;
    fn layer(&self, inner: S) -> Self::Service {
        TcpRpcPathGate { inner }
    }
}

#[derive(Clone)]
struct TcpRpcPathGate<S> {
    inner: S,
}

impl<S> Service<http::Request<tonic::body::BoxBody>> for TcpRpcPathGate<S>
where
    S: Service<
            http::Request<tonic::body::BoxBody>,
            Response = http::Response<tonic::body::BoxBody>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: http::Request<tonic::body::BoxBody>) -> Self::Future {
        let path = request.uri().path();
        if !tcp_rpc_path_allowed(path) {
            let response = http::Response::builder()
                .status(http::StatusCode::FORBIDDEN)
                .body(tonic::body::empty_body())
                .expect("static forbidden response is valid");
            return Box::pin(async move { Ok(response) });
        }
        let future = self.inner.call(request);
        Box::pin(future)
    }
}

#[derive(Clone, Copy)]
struct BffRpcPathLayer;

impl<S> Layer<S> for BffRpcPathLayer {
    type Service = BffRpcPathGate<S>;
    fn layer(&self, inner: S) -> Self::Service {
        BffRpcPathGate { inner }
    }
}

#[derive(Clone)]
struct BffRpcPathGate<S> {
    inner: S,
}

impl<S> Service<http::Request<tonic::body::BoxBody>> for BffRpcPathGate<S>
where
    S: Service<
            http::Request<tonic::body::BoxBody>,
            Response = http::Response<tonic::body::BoxBody>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }
    fn call(&mut self, request: http::Request<tonic::body::BoxBody>) -> Self::Future {
        let path = request.uri().path();
        if !bff_rpc_path_allowed(path) {
            let response = http::Response::builder()
                .status(http::StatusCode::FORBIDDEN)
                .body(tonic::body::empty_body())
                .expect("static forbidden response is valid");
            return Box::pin(async move { Ok(response) });
        }
        Box::pin(self.inner.call(request))
    }
}

fn tcp_rpc_path_allowed(path: &str) -> bool {
    matches!(path,
        "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/NegotiateVersion"
        | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/ClaimInvocations"
        | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/ValidateExecutionAuthorization"
        | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/AcknowledgeDelivery"
        | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/SubmitInvocationResult"
    )
}

fn bff_rpc_path_allowed(path: &str) -> bool {
    matches!(
        path,
        "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/NegotiateVersion"
            | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/VerifyIdentity"
            | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/GetCatalogSnapshot"
            | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/ApproveAndEnqueueInvocation"
            | "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/WaitInvocationResult"
    )
}

pub async fn run_admin_cli() -> Result<(), HostError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().ok_or(HostError::Configuration)?;
    if arguments.next().is_some() {
        return Err(HostError::Configuration);
    }
    let command = command.to_str().ok_or(HostError::Configuration)?;
    let action = match command {
        "status" => "status",
        "validate-artifact" => "validateArtifact",
        "activate-artifact" => "activateArtifact",
        _ => return Err(HostError::Configuration),
    };
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .await
        .map_err(|_| HostError::Configuration)?;
    if line.len() > MAX_ADMIN_LINE || !line.ends_with('\n') {
        return Err(HostError::Configuration);
    }
    let request: AdminRequest =
        serde_json::from_str(&line).map_err(|_| HostError::Configuration)?;
    if request.action != action {
        return Err(HostError::Configuration);
    }
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::UnixStream::connect(ADMIN_UDS_DEFAULT),
    )
    .await
    .map_err(|_| {
        HostError::Listener(io::Error::new(
            io::ErrorKind::TimedOut,
            "admin socket timeout",
        ))
    })??;
    stream.write_all(line.as_bytes()).await?;
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(120),
        BufReader::new(stream).read_line(&mut response),
    )
    .await
    .map_err(|_| {
        HostError::Listener(io::Error::new(
            io::ErrorKind::TimedOut,
            "admin response timeout",
        ))
    })??;
    if response.len() > MAX_ADMIN_LINE || !response.ends_with('\n') {
        return Err(HostError::Configuration);
    }
    let response_value: Value =
        serde_json::from_str(&response).map_err(|_| HostError::Configuration)?;
    print!("{response}");
    if response_value.get("status").and_then(Value::as_str) == Some("error") {
        return Err(HostError::Configuration);
    }
    Ok(())
}

fn prepare_private_socket_parent(path: &Path) -> Result<(), HostError> {
    let parent = path.parent().ok_or(HostError::Configuration)?;
    for ancestor in parent.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(HostError::ProtectedFile);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(HostError::Listener(error)),
        }
    }
    fs::create_dir_all(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err(HostError::ProtectedFile);
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err(HostError::ProtectedFile);
    }
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(HostError::ProtectedFile);
        }
    }
    Ok(())
}

fn bind_private_uds(path: &Path) -> Result<UnixListener, HostError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
            return Err(HostError::ProtectedFile);
        }
        fs::remove_file(path)?;
    }
    let std_listener = StdUnixListener::bind(path)?;
    std_listener.set_nonblocking(true)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(UnixListener::from_std(std_listener)?)
}

fn read_secret_file(path: &Path, max: usize) -> Result<Vec<u8>, HostError> {
    let metadata = fs::symlink_metadata(path)?;
    let uid = nix::unistd::geteuid().as_raw();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() as usize > max
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != uid
    {
        return Err(HostError::ProtectedFile);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file()
        || opened.len() as usize > max
        || opened.mode() & 0o077 != 0
        || opened.uid() != uid
    {
        return Err(HostError::ProtectedFile);
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(HostError::ProtectedFile);
    }
    Ok(bytes)
}

fn read_protected_file(path: &Path, max: usize) -> Result<Vec<u8>, HostError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() as usize > max
        || metadata.mode() & 0o027 != 0
        || metadata.uid() != 0
    {
        return Err(HostError::ProtectedFile);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file()
        || opened_metadata.len() as usize > max
        || opened_metadata.uid() != 0
        || opened_metadata.mode() & 0o027 != 0
    {
        return Err(HostError::ProtectedFile);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(HostError::ProtectedFile);
    }
    Ok(bytes)
}

fn validate_private_directory(path: &Path) -> Result<(), HostError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.mode() & 0o027 != 0
        || metadata.uid() != 0
    {
        return Err(HostError::ProtectedFile);
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(HostError::ProtectedFile);
        }
    }
    Ok(())
}

fn env_value(name: &str, default: &str) -> Result<String, HostError> {
    let value = env::var(name).unwrap_or_else(|_| default.to_owned());
    if value.trim() != value || value.is_empty() {
        return Err(HostError::Configuration);
    }
    Ok(value)
}
fn path_value(name: &str, default: &str) -> Result<PathBuf, HostError> {
    Ok(PathBuf::from(env_value(name, default)?))
}
fn path_env(name: &str) -> Result<PathBuf, HostError> {
    let value = env::var(name).map_err(|_| HostError::Configuration)?;
    if value.trim() != value || value.is_empty() {
        return Err(HostError::Configuration);
    }
    Ok(PathBuf::from(value))
}
fn loopback_addr(value: String) -> Result<String, HostError> {
    let address = value
        .parse::<std::net::SocketAddr>()
        .map_err(|_| HostError::Configuration)?;
    if !address.ip().is_loopback() {
        return Err(HostError::Configuration);
    }
    Ok(value)
}
fn artifact_hex(value: &str) -> Option<&str> {
    value.strip_prefix("sha256:").filter(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
fn is_artifact_id(value: &str) -> bool {
    artifact_hex(value).is_some()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        bff_rpc_path_allowed, is_artifact_id, tcp_rpc_path_allowed, verify_outer_manifest,
    };
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};

    fn manifest_with_digest() -> Value {
        let mut manifest = json!({
            "schemaVersion": 2,
            "componentId": "workspace-product-contract-bundle",
            "artifact": {"kind": "data-bundle", "sha256": format!("sha256:{}", "a".repeat(64))},
            "contentDigest": format!("sha256:{}", "a".repeat(64)),
            "dataBundle": {"proofPath": "data-bundle-proof-v1.json", "proofSha256": format!("sha256:{}", "b".repeat(64))}
        });
        let digest = format!(
            "sha256:{}",
            Sha256::digest(serde_jcs::to_vec(&manifest).unwrap())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        manifest["manifestDigest"] = json!(digest);
        manifest
    }

    #[test]
    fn tcp_and_bff_rpc_routes_are_disjoint_except_version_negotiation() {
        let prefix = "/cyrene.workspace.authority.v2.WorkspaceAuthorityService/";
        assert!(tcp_rpc_path_allowed(&format!("{prefix}ClaimInvocations")));
        assert!(!tcp_rpc_path_allowed(&format!(
            "{prefix}ApproveAndEnqueueInvocation"
        )));
        assert!(bff_rpc_path_allowed(&format!(
            "{prefix}ApproveAndEnqueueInvocation"
        )));
        assert!(!bff_rpc_path_allowed(&format!("{prefix}ClaimInvocations")));
        assert!(tcp_rpc_path_allowed(&format!("{prefix}NegotiateVersion")));
        assert!(bff_rpc_path_allowed(&format!("{prefix}NegotiateVersion")));
    }

    #[test]
    fn artifact_ids_require_lowercase_sha256() {
        assert!(is_artifact_id(&format!("sha256:{}", "0a".repeat(32))));
        assert!(!is_artifact_id(&format!("sha256:{}", "0A".repeat(32))));
        assert!(!is_artifact_id(&format!("sha256:{}", "a".repeat(63))));
    }

    #[test]
    fn outer_manifest_digest_and_archive_bindings_are_checked() {
        let manifest = manifest_with_digest();
        let artifact_id = format!("sha256:{}", "a".repeat(64));
        assert!(verify_outer_manifest(&manifest, &artifact_id).is_ok());
        assert!(verify_outer_manifest(&manifest, &format!("sha256:{}", "c".repeat(64))).is_err());
        let mut changed = manifest;
        changed["dataBundle"]["proofPath"] = json!("other.json");
        assert!(verify_outer_manifest(&changed, &artifact_id).is_err());
    }
}
