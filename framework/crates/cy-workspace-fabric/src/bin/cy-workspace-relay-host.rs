//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-relay-host.rs                                      │
//! │  Module: cy_workspace_fabric::relay_host                            │
//! │  Role: Fail-closed non-fixture Workspace Relay process host.        │
//! │                                                                     │
//! │  模块职责：默认拒绝 Relay 会话的非 fixture 服务进程入口。                │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! The host owns only Workspace relay transport and reads membership and
//! discovery from the PostgreSQL Directory. It does not create Product,
//! Kernel, Runtime, or Artifact authority. Frontend BFF authentication is an
//! explicit ACA XFCC plus signed-handoff configuration; WorkspaceConnector
//! authentication remains disabled until the durable device registry is wired.

use std::env;
use std::error::Error;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use cy_observability::{init_observability, ObservabilityConfig};
use cy_proto::workspace_v1::UserIdentityRef;
use cy_workspace_fabric::{
    bounded_workspace_relay_server, AcaForwardedBffWorkloadCertificateAdapter,
    BffWorkloadCertificatePin, DurableDirectoryError, PostgresWorkspaceDirectory,
    RelayAuthenticationError, RelayAuthenticator, RelaySessionClaims, WebRelaySessionVerifier,
    WorkspaceRelay,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::timeout;
use tonic::transport::Server;

const DEFAULT_RELAY_BIND: &str = "127.0.0.1:8080";
const DEFAULT_HEALTH_BIND: &str = "127.0.0.1:8081";
const ACA_INGRESS_ASSERTION: &str = "client-certificate-required";
const BFF_CLIENT_CA_BUNDLE_ENV: &str = "CYRENE_WORKSPACE_RELAY_BFF_CLIENT_CA_BUNDLE";
const BFF_CERTIFICATE_ALLOWLIST_ENV: &str = "CYRENE_WORKSPACE_RELAY_BFF_CERT_ALLOWLIST";
const WEB_HANDOFF_ISSUER_ENV: &str = "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_ISSUER";
const WEB_HANDOFF_AUDIENCE_ENV: &str = "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_AUDIENCE";
const WEB_HANDOFF_PUBLIC_KEY_ENV: &str = "CYRENE_WORKSPACE_RELAY_WEB_HANDOFF_PUBLIC_KEY_BASE64URL";
const HEALTH_BIND_ASSERTION: &str = "probe-only-not-ingress";
const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const DIRECTORY_READINESS_TIMEOUT: Duration = Duration::from_millis(1500);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_HEALTH_REQUEST_BYTES: usize = 2048;
const MAX_HEALTH_CONNECTIONS: usize = 32;
const MAX_BFF_CLIENT_CA_BUNDLE_BYTES: u64 = 1024 * 1024;
const MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES: u64 = 64 * 1024;
const READINESS_PROBE_ISSUER: &str = "https://relay-readiness.invalid";
const READINESS_PROBE_SUBJECT: &str = "workspace-directory-probe";

struct FrontendWorkloadConfig {
    client_ca_bundle: PathBuf,
    certificate_allowlist: PathBuf,
    handoff_issuer: String,
    handoff_audience: String,
    handoff_public_key: [u8; 32],
}

struct HostConfig {
    relay_bind: SocketAddr,
    health_bind: SocketAddr,
    frontend_workload: Option<FrontendWorkloadConfig>,
}

impl HostConfig {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        reject_fixture_credentials()?;

        let relay_bind = socket_addr_from_env("CYRENE_WORKSPACE_RELAY_BIND", DEFAULT_RELAY_BIND)?;
        let health_bind =
            socket_addr_from_env("CYRENE_WORKSPACE_RELAY_HEALTH_BIND", DEFAULT_HEALTH_BIND)?;
        let ingress_assertion =
            match env::var_os("CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION") {
                Some(value) => Some(value.into_string().map_err(|_| {
                    "CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION must be valid UTF-8"
                })?),
                None => None,
            };
        let health_assertion = env::var("CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION").ok();
        let aca_ingress_enabled =
            validate_aca_ingress_config(relay_bind, ingress_assertion.as_deref())?;
        let frontend_workload = frontend_workload_config(aca_ingress_enabled)?;
        if health_assertion
            .as_deref()
            .is_some_and(|assertion| assertion != HEALTH_BIND_ASSERTION)
        {
            return Err(format!(
                "CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION must be '{HEALTH_BIND_ASSERTION}'"
            )
            .into());
        }
        if !health_bind.ip().is_loopback()
            && health_assertion.as_deref() != Some(HEALTH_BIND_ASSERTION)
        {
            return Err(format!(
                "non-loopback health bind {health_bind} requires CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION={HEALTH_BIND_ASSERTION}"
            )
            .into());
        }

        Ok(Self {
            relay_bind,
            health_bind,
            frontend_workload,
        })
    }
}

fn frontend_workload_config(
    aca_ingress_enabled: bool,
) -> Result<Option<FrontendWorkloadConfig>, Box<dyn Error>> {
    parse_frontend_workload_config(
        aca_ingress_enabled,
        env::var_os(BFF_CLIENT_CA_BUNDLE_ENV).map(PathBuf::from),
        env::var_os(BFF_CERTIFICATE_ALLOWLIST_ENV).map(PathBuf::from),
        optional_nonempty_env(WEB_HANDOFF_ISSUER_ENV)?,
        optional_nonempty_env(WEB_HANDOFF_AUDIENCE_ENV)?,
        optional_nonempty_env(WEB_HANDOFF_PUBLIC_KEY_ENV)?,
    )
}

fn parse_frontend_workload_config(
    aca_ingress_enabled: bool,
    client_ca_bundle: Option<PathBuf>,
    certificate_allowlist: Option<PathBuf>,
    handoff_issuer: Option<String>,
    handoff_audience: Option<String>,
    handoff_public_key: Option<String>,
) -> Result<Option<FrontendWorkloadConfig>, Box<dyn Error>> {
    let values_present = [
        client_ca_bundle.is_some(),
        certificate_allowlist.is_some(),
        handoff_issuer.is_some(),
        handoff_audience.is_some(),
        handoff_public_key.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    if values_present == 0 {
        return Ok(None);
    }
    if values_present != 5 {
        return Err(format!(
            "BFF Frontend authentication requires all of {BFF_CLIENT_CA_BUNDLE_ENV}, {BFF_CERTIFICATE_ALLOWLIST_ENV}, {WEB_HANDOFF_ISSUER_ENV}, {WEB_HANDOFF_AUDIENCE_ENV}, and {WEB_HANDOFF_PUBLIC_KEY_ENV}"
        )
        .into());
    }
    if !aca_ingress_enabled {
        return Err(format!(
        "BFF Frontend authentication requires CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION={ACA_INGRESS_ASSERTION}"
        )
        .into());
    }
    if client_ca_bundle
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
        || certificate_allowlist
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err("BFF certificate configuration paths must not be empty".into());
    }

    let encoded_key = handoff_public_key.ok_or("BFF handoff public key is required")?;
    let decoded_key = URL_SAFE_NO_PAD.decode(encoded_key.as_bytes())?;
    if URL_SAFE_NO_PAD.encode(&decoded_key) != encoded_key {
        return Err(format!(
            "{WEB_HANDOFF_PUBLIC_KEY_ENV} must use canonical base64url without padding"
        )
        .into());
    }
    let handoff_public_key: [u8; 32] = decoded_key
        .try_into()
        .map_err(|_| format!("{WEB_HANDOFF_PUBLIC_KEY_ENV} must decode to exactly 32 bytes"))?;

    Ok(Some(FrontendWorkloadConfig {
        client_ca_bundle: client_ca_bundle.ok_or("BFF client CA bundle is required")?,
        certificate_allowlist: certificate_allowlist
            .ok_or("BFF certificate allowlist is required")?,
        handoff_issuer: handoff_issuer.ok_or("BFF handoff issuer is required")?,
        handoff_audience: handoff_audience.ok_or("BFF handoff audience is required")?,
        handoff_public_key,
    }))
}

fn optional_nonempty_env(name: &str) -> Result<Option<String>, Box<dyn Error>> {
    let Some(value) = env::var_os(name) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| format!("{name} must be valid UTF-8"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(Some(value))
}

fn validate_aca_ingress_config(
    relay_bind: SocketAddr,
    ingress_assertion: Option<&str>,
) -> Result<bool, Box<dyn Error>> {
    if ingress_assertion.is_some_and(|value| value != ACA_INGRESS_ASSERTION) {
        return Err(format!(
            "CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION must be '{ACA_INGRESS_ASSERTION}'"
        )
        .into());
    }

    if !relay_bind.ip().is_loopback() && ingress_assertion.is_none() {
        return Err(format!(
            "non-loopback relay bind {relay_bind} requires the explicit ACA ingress assertion"
        )
        .into());
    }

    Ok(ingress_assertion.is_some())
}

fn read_bff_client_ca_bundle(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BFF_CLIENT_CA_BUNDLE_BYTES
    {
        return Err(format!(
            "BFF workload client CA bundle must be a regular file no larger than {MAX_BFF_CLIENT_CA_BUNDLE_BYTES} bytes"
        )
        .into());
    }

    let mut contents = Vec::with_capacity(usize::try_from(metadata.len())?);
    file.take(MAX_BFF_CLIENT_CA_BUNDLE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.is_empty() || contents.len() as u64 > MAX_BFF_CLIENT_CA_BUNDLE_BYTES {
        return Err(format!(
            "BFF workload client CA bundle must not exceed {MAX_BFF_CLIENT_CA_BUNDLE_BYTES} bytes"
        )
        .into());
    }
    Ok(contents)
}

fn read_bff_certificate_allowlist(
    path: &Path,
) -> Result<Vec<BffWorkloadCertificatePin>, Box<dyn Error>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES
    {
        return Err(format!(
            "BFF certificate allowlist must be a regular file no larger than {MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES} bytes"
        )
        .into());
    }

    let mut contents = Vec::with_capacity(usize::try_from(metadata.len())?);
    file.take(MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.is_empty() || contents.len() as u64 > MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES {
        return Err(format!(
            "BFF certificate allowlist must not exceed {MAX_BFF_CERTIFICATE_ALLOWLIST_BYTES} bytes"
        )
        .into());
    }
    Ok(serde_json::from_slice(&contents)?)
}

fn socket_addr_from_env(name: &str, default: &str) -> Result<SocketAddr, Box<dyn Error>> {
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => default.to_string(),
        Err(error) => return Err(error.into()),
    };
    Ok(value.parse()?)
}

fn reject_fixture_credentials() -> Result<(), Box<dyn Error>> {
    for name in [
        "CYRENE_FRONTEND_SESSION_CREDENTIAL",
        "CYRENE_WORKSPACE_SESSION_CREDENTIAL",
    ] {
        if env::var_os(name).is_some() {
            return Err(format!(
                "fixture credential environment variable {name} is not accepted by the relay host"
            )
            .into());
        }
    }
    Ok(())
}

/// Frontend user-session authentication remains unavailable until its production verifier is wired.
struct UnavailableFrontendIdentity;

impl RelayAuthenticator for UnavailableFrontendIdentity {
    fn authenticate(
        &self,
        _hello: &cy_proto::workspace_v1::RelayHello,
        _now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, RelayAuthenticationError> {
        Err(RelayAuthenticationError::InvalidCredential)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let log_level = env::var("CYRENE_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let observability = init_observability(
        ObservabilityConfig::managed("cy-workspace-relay-host").with_log_level(log_level),
    )?;

    let result = run_host().await;
    if let Err(error) = &result {
        tracing::error!(
            event.name = "platform.workspace_relay.host_failed",
            error = %error,
            message = "Workspace Relay host stopped with an error",
        );
    }
    observability.shutdown_with_timeout(GRACEFUL_SHUTDOWN_TIMEOUT);
    result
}

async fn run_host() -> Result<(), Box<dyn Error>> {
    let config = HostConfig::from_env()?;
    let directory = Arc::new(PostgresWorkspaceDirectory::connect_from_environment().await?);
    let authenticator: Arc<dyn RelayAuthenticator> = match &config.frontend_workload {
        Some(frontend_config) => Arc::new(WebRelaySessionVerifier::new(
            frontend_config.handoff_issuer.clone(),
            frontend_config.handoff_audience.clone(),
            frontend_config.handoff_public_key,
        )?),
        None => Arc::new(UnavailableFrontendIdentity),
    };
    let relay = if let Some(frontend_config) = &config.frontend_workload {
        let bff_ca_bundle = read_bff_client_ca_bundle(&frontend_config.client_ca_bundle)?;
        let certificate_pins =
            read_bff_certificate_allowlist(&frontend_config.certificate_allowlist)?;
        let frontend_adapter =
            AcaForwardedBffWorkloadCertificateAdapter::new(&bff_ca_bundle, certificate_pins)?;
        WorkspaceRelay::with_aca_forwarded_frontend_certificate_adapter(
            directory.clone(),
            authenticator,
            frontend_adapter,
        )
    } else {
        WorkspaceRelay::new(directory.clone(), authenticator)
    };
    let health_listener = TcpListener::bind(config.health_bind).await?;
    let readiness = Arc::new(RelayReadiness {
        directory: directory.clone(),
        frontend_authentication_configured: config.frontend_workload.is_some(),
    });

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let relay_shutdown = shutdown_rx.clone();
    let health_shutdown = shutdown_rx;
    let relay_server = Server::builder()
        .add_service(bounded_workspace_relay_server(relay))
        .serve_with_shutdown(config.relay_bind, wait_for_shutdown(relay_shutdown));
    let health_server = serve_health(
        health_listener,
        readiness,
        wait_for_shutdown(health_shutdown),
    );
    tokio::pin!(relay_server);
    tokio::pin!(health_server);
    let signal = shutdown_signal();
    tokio::pin!(signal);

    tracing::info!(
        event.name = "platform.workspace_relay.host_started",
        relay_bind = %config.relay_bind,
        health_bind = %config.health_bind,
        frontend_authentication = if config.frontend_workload.is_some() {
            "bff_workload_certificate_and_signed_handoff"
        } else {
            "deny_all_without_bff_workload_and_handoff_config"
        },
        workspace_directory = "postgresql",
        workspace_device_authentication = "disabled_until_durable_device_registry_is_composed",
        ready = false,
        message = "Workspace Relay host is listening with fail-closed authentication",
    );
    tracing::warn!(
        event.name = "platform.workspace_relay.production_gate",
        error.code = "RELAY_PRODUCTION_ACCESS_CONTROL_UNAVAILABLE",
        message = "Directory membership is backed by PostgreSQL; Directory administration, durable device registry, DeviceAuthorization, CA issuance, WebAuthn, Product private access, and verified deployment topology remain unavailable; /readyz remains unavailable",
    );

    enum HostExit {
        Signal(io::Result<&'static str>),
        Relay(Result<(), tonic::transport::Error>),
        Health(io::Result<()>),
    }

    let exit = tokio::select! {
        result = &mut signal => HostExit::Signal(result),
        result = &mut relay_server => HostExit::Relay(result),
        result = &mut health_server => HostExit::Health(result),
    };
    let _ = shutdown_tx.send(true);

    match exit {
        HostExit::Signal(Ok(signal_name)) => {
            tracing::info!(
                event.name = "platform.workspace_relay.shutdown_started",
                signal = signal_name,
                message = "Graceful Relay host shutdown started",
            );
            let (relay_result, health_result) = timeout(GRACEFUL_SHUTDOWN_TIMEOUT, async {
                tokio::join!(&mut relay_server, &mut health_server)
            })
            .await?;
            relay_result?;
            health_result?;
        }
        HostExit::Signal(Err(error)) => {
            tracing::error!(
                event.name = "platform.workspace_relay.shutdown_signal_failed",
                error = %error,
                message = "Signal handler failed; stopping listeners fail-closed",
            );
            let (relay_result, health_result) = timeout(GRACEFUL_SHUTDOWN_TIMEOUT, async {
                tokio::join!(&mut relay_server, &mut health_server)
            })
            .await?;
            relay_result?;
            health_result?;
            return Err(error.into());
        }
        HostExit::Relay(result) => {
            let health_result = timeout(GRACEFUL_SHUTDOWN_TIMEOUT, &mut health_server).await?;
            health_result?;
            result?;
            return Err("Relay listener stopped before a shutdown signal".into());
        }
        HostExit::Health(result) => {
            let relay_result = timeout(GRACEFUL_SHUTDOWN_TIMEOUT, &mut relay_server).await?;
            relay_result?;
            result?;
            return Err("health listener stopped before a shutdown signal".into());
        }
    }

    tracing::info!(
        event.name = "platform.workspace_relay.shutdown_complete",
        message = "Workspace Relay host stopped gracefully",
    );
    Ok(())
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

struct RelayReadiness {
    directory: Arc<PostgresWorkspaceDirectory>,
    frontend_authentication_configured: bool,
}

impl RelayReadiness {
    async fn response(&self) -> (u16, &'static str, String) {
        let probe = UserIdentityRef {
            issuer: READINESS_PROBE_ISSUER.to_string(),
            subject: READINESS_PROBE_SUBJECT.to_string(),
        };
        let directory_available = timeout(
            DIRECTORY_READINESS_TIMEOUT,
            self.directory.organizations_for_verified_identity(&probe),
        )
        .await
        .is_ok_and(|result: Result<Vec<String>, DurableDirectoryError>| result.is_ok());
        let workspace_device_registry = false;
        let directory_administration = false;
        let device_authorization = false;
        let certificate_issuance = false;
        let webauthn = false;
        let product_private_access = false;
        let deployment_topology_verified = false;
        let ready = directory_available
            && self.frontend_authentication_configured
            && workspace_device_registry
            && directory_administration
            && device_authorization
            && certificate_issuance
            && webauthn
            && product_private_access
            && deployment_topology_verified;
        let (status, reason, state) = if ready {
            (200, "OK", "ready")
        } else {
            (503, "Service Unavailable", "not_ready")
        };
        let body = format!(
            r#"{{"status":"{}","checks":{{"directoryDatabase":{},"frontendAuthenticationConfigured":{},"directoryAdministration":{},"workspaceDeviceRegistry":{},"deviceAuthorization":{},"certificateIssuance":{},"webauthn":{},"productPrivateAccess":{},"deploymentTopologyVerified":{}}}}}"#,
            state,
            directory_available,
            self.frontend_authentication_configured,
            directory_administration,
            workspace_device_registry,
            device_authorization,
            certificate_issuance,
            webauthn,
            product_private_access,
            deployment_topology_verified,
        );
        (status, reason, body)
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<&'static str> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map(|()| "SIGINT"),
        _ = terminate.recv() => Ok("SIGTERM"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> io::Result<&'static str> {
    tokio::signal::ctrl_c().await.map(|()| "CTRL_C")
}

async fn serve_health<F>(
    listener: TcpListener,
    readiness: Arc<RelayReadiness>,
    shutdown: F,
) -> io::Result<()>
where
    F: std::future::Future<Output = ()>,
{
    let connections = Arc::new(Semaphore::new(MAX_HEALTH_CONNECTIONS));
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    continue;
                };
                let readiness = Arc::clone(&readiness);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_health_request(stream, readiness).await {
                        tracing::debug!(
                            event.name = "platform.workspace_relay.health_probe_rejected",
                            peer = %peer,
                            error = %error,
                            message = "Health probe request could not be served",
                        );
                    }
                });
            }
        }
    }
}

async fn handle_health_request(
    mut stream: TcpStream,
    readiness: Arc<RelayReadiness>,
) -> io::Result<()> {
    let request = timeout(HEALTH_REQUEST_TIMEOUT, read_health_request(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "health request timed out"))??;
    let request_line = request.lines().next().unwrap_or_default();
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();

    let (status, reason, body) = if method != "GET" || !version.starts_with("HTTP/1.") {
        (
            400,
            "Bad Request",
            r#"{"status":"bad_request"}"#.to_string(),
        )
    } else {
        match path {
            "/healthz" => (200, "OK", r#"{"status":"alive"}"#.to_string()),
            "/readyz" => readiness.response().await,
            _ => (404, "Not Found", r#"{"status":"not_found"}"#.to_string()),
        }
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_health_request(stream: &mut TcpStream) -> io::Result<String> {
    let mut request = Vec::with_capacity(256);
    let mut chunk = [0_u8; 256];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "health request ended before headers",
            ));
        }
        if request.len().saturating_add(read) > MAX_HEALTH_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "health request headers exceed the size limit",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(request)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP header"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontend_workload_values(
        aca_ingress_enabled: bool,
        public_key: String,
    ) -> Result<Option<FrontendWorkloadConfig>, Box<dyn Error>> {
        parse_frontend_workload_config(
            aca_ingress_enabled,
            Some(PathBuf::from("/etc/cyrene/bff-ca/roots.pem")),
            Some(PathBuf::from("/etc/cyrene/bff-ca/allowlist.json")),
            Some("https://workspace-web-bff.internal".to_string()),
            Some("cyrene-workspace-relay".to_string()),
            Some(public_key),
        )
    }

    fn loopback_bind() -> SocketAddr {
        "127.0.0.1:8080".parse().unwrap()
    }

    #[test]
    fn default_loopback_configuration_does_not_select_aca_identity() {
        assert!(!validate_aca_ingress_config(loopback_bind(), None).unwrap());
    }

    #[test]
    fn bff_identity_requires_the_explicit_aca_ingress_assertion() {
        let key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        assert!(frontend_workload_values(false, key)
            .err()
            .unwrap()
            .to_string()
            .contains("CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION"));
    }

    #[test]
    fn unknown_ingress_assertion_is_rejected() {
        assert!(
            validate_aca_ingress_config(loopback_bind(), Some("trust-forwarded-headers"))
                .unwrap_err()
                .to_string()
                .contains(ACA_INGRESS_ASSERTION)
        );
    }

    #[test]
    fn non_loopback_bind_requires_explicit_aca_ingress_assertion() {
        let bind = "0.0.0.0:8080".parse().unwrap();
        assert!(validate_aca_ingress_config(bind, None)
            .unwrap_err()
            .to_string()
            .contains("ACA ingress assertion"));
        assert!(validate_aca_ingress_config(bind, Some(ACA_INGRESS_ASSERTION)).unwrap());
    }

    #[test]
    fn bff_ca_bundle_reader_enforces_its_size_limit() {
        let directory = tempfile::tempdir().unwrap();
        let bundle_path = directory.path().join("roots.pem");
        std::fs::write(&bundle_path, b"trusted roots").unwrap();
        assert_eq!(
            read_bff_client_ca_bundle(&bundle_path).unwrap(),
            b"trusted roots"
        );

        let oversized = vec![0_u8; usize::try_from(MAX_BFF_CLIENT_CA_BUNDLE_BYTES + 1).unwrap()];
        std::fs::write(&bundle_path, oversized).unwrap();
        assert!(read_bff_client_ca_bundle(&bundle_path).is_err());
    }

    #[test]
    fn frontend_workload_configuration_is_all_or_nothing_and_requires_aca() {
        assert!(
            parse_frontend_workload_config(false, None, None, None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(parse_frontend_workload_config(
            true,
            None,
            None,
            Some("issuer".to_string()),
            None,
            None,
        )
        .is_err());

        let key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        assert!(frontend_workload_values(false, key.clone()).is_err());
        assert!(frontend_workload_values(true, key).unwrap().is_some());
    }

    #[test]
    fn frontend_handoff_public_key_must_be_canonical_ed25519_bytes() {
        assert!(frontend_workload_values(true, URL_SAFE_NO_PAD.encode([1_u8; 31])).is_err());
        assert!(
            frontend_workload_values(true, format!("{}=", URL_SAFE_NO_PAD.encode([1_u8; 32])))
                .is_err()
        );
    }
}
