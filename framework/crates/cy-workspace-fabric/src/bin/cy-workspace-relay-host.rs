//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-relay-host.rs                                      │
//! │  Module: cy_workspace_fabric::relay_host                            │
//! │  Role: Fail-closed non-fixture Workspace Relay process host.        │
//! │                                                                     │
//! │  模块职责：默认拒绝 Relay 会话的非 fixture 服务进程入口。                │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! The host owns only Workspace relay transport and its persistent directory
//! snapshot. It does not create Product, Kernel, Runtime, or Artifact authority.
//! ACA XFCC device identity is an explicit opt-in, while frontend identity,
//! DeviceAuthorization, CA issuance, WebAuthn, and Product access remain unready.

use std::env;
use std::error::Error;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cy_observability::{init_observability, ObservabilityConfig};
use cy_workspace_fabric::{
    bounded_workspace_relay_server, AcaForwardedCertificateAdapter, FileWorkspaceDirectory,
    RelayAuthenticationError, RelayAuthenticator, RelaySessionClaims, WorkspaceRelay,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::timeout;
use tonic::transport::Server;

const DEFAULT_RELAY_BIND: &str = "127.0.0.1:8080";
const DEFAULT_HEALTH_BIND: &str = "127.0.0.1:8081";
const ACA_INGRESS_ASSERTION: &str = "client-certificate-required";
const ACA_CLIENT_CA_BUNDLE_ENV: &str = "CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE";
const HEALTH_BIND_ASSERTION: &str = "probe-only-not-ingress";
const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_HEALTH_REQUEST_BYTES: usize = 2048;
const MAX_HEALTH_CONNECTIONS: usize = 32;
const MAX_ACA_CLIENT_CA_BUNDLE_BYTES: u64 = 1024 * 1024;
const NOT_READY_BODY: &str = r#"{"status":"not_ready","reason":"frontend_identity_directory_admin_ca_webauthn_and_product_dependencies_unavailable"}"#;

struct HostConfig {
    relay_bind: SocketAddr,
    health_bind: SocketAddr,
    directory: PathBuf,
    aca_client_ca_bundle: Option<PathBuf>,
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
        let configured_ca_bundle = env::var_os(ACA_CLIENT_CA_BUNDLE_ENV).map(PathBuf::from);
        let health_assertion = env::var("CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION").ok();
        let aca_client_ca_bundle = validate_aca_ingress_config(
            relay_bind,
            ingress_assertion.as_deref(),
            configured_ca_bundle,
        )?;
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

        let directory = env::var_os("CYRENE_WORKSPACE_RELAY_DIRECTORY")
            .map(PathBuf::from)
            .ok_or("CYRENE_WORKSPACE_RELAY_DIRECTORY must name a private persistent directory")?;

        Ok(Self {
            relay_bind,
            health_bind,
            directory,
            aca_client_ca_bundle,
        })
    }
}

fn validate_aca_ingress_config(
    relay_bind: SocketAddr,
    ingress_assertion: Option<&str>,
    ca_bundle: Option<PathBuf>,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if ingress_assertion.is_some_and(|value| value != ACA_INGRESS_ASSERTION) {
        return Err(format!(
            "CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION must be '{ACA_INGRESS_ASSERTION}'"
        )
        .into());
    }

    let selected_ca_bundle = match (ingress_assertion, ca_bundle) {
        (Some(_), Some(path)) if !path.as_os_str().is_empty() => Some(path),
        (Some(_), None) => {
            return Err(format!(
                "ACA ingress requires {ACA_CLIENT_CA_BUNDLE_ENV} with a private client CA bundle"
            )
            .into());
        }
        (Some(_), Some(_)) => {
            return Err(format!("{ACA_CLIENT_CA_BUNDLE_ENV} must not be empty").into());
        }
        (None, Some(_)) => {
            return Err(format!(
                "{ACA_CLIENT_CA_BUNDLE_ENV} requires CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION={ACA_INGRESS_ASSERTION}"
            )
            .into());
        }
        (None, None) => None,
    };

    if !relay_bind.ip().is_loopback() && selected_ca_bundle.is_none() {
        return Err(format!(
            "non-loopback relay bind {relay_bind} requires an ACA client CA bundle and the explicit ACA ingress assertion"
        )
        .into());
    }

    Ok(selected_ca_bundle)
}

fn read_aca_client_ca_bundle(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ACA_CLIENT_CA_BUNDLE_BYTES
    {
        return Err(format!(
            "ACA client CA bundle must be a regular file no larger than {MAX_ACA_CLIENT_CA_BUNDLE_BYTES} bytes"
        )
        .into());
    }

    let mut contents = Vec::with_capacity(usize::try_from(metadata.len())?);
    file.take(MAX_ACA_CLIENT_CA_BUNDLE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.is_empty() || contents.len() as u64 > MAX_ACA_CLIENT_CA_BUNDLE_BYTES {
        return Err(format!(
            "ACA client CA bundle must not exceed {MAX_ACA_CLIENT_CA_BUNDLE_BYTES} bytes"
        )
        .into());
    }
    Ok(contents)
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
    let directory = Arc::new(FileWorkspaceDirectory::open(&config.directory)?);
    let aca_ingress_enabled = config.aca_client_ca_bundle.is_some();
    let authenticator = Arc::new(UnavailableFrontendIdentity);
    let relay = if let Some(bundle_path) = &config.aca_client_ca_bundle {
        let ca_bundle = read_aca_client_ca_bundle(bundle_path)?;
        let adapter = AcaForwardedCertificateAdapter::new(&ca_bundle)?;
        WorkspaceRelay::with_aca_forwarded_certificate_adapter(
            directory.clone(),
            authenticator,
            directory.clone(),
            adapter,
        )
    } else {
        WorkspaceRelay::new(directory, authenticator)
    };
    let health_listener = TcpListener::bind(config.health_bind).await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let relay_shutdown = shutdown_rx.clone();
    let health_shutdown = shutdown_rx;
    let relay_server = Server::builder()
        .add_service(bounded_workspace_relay_server(relay))
        .serve_with_shutdown(config.relay_bind, wait_for_shutdown(relay_shutdown));
    let health_server = serve_health(health_listener, wait_for_shutdown(health_shutdown));
    tokio::pin!(relay_server);
    tokio::pin!(health_server);
    let signal = shutdown_signal();
    tokio::pin!(signal);

    tracing::info!(
        event.name = "platform.workspace_relay.host_started",
        relay_bind = %config.relay_bind,
        health_bind = %config.health_bind,
        frontend_authentication = "deny_all_until_production_user_identity",
        workspace_device_authentication = if aca_ingress_enabled { "aca_xfcc_with_file_registry" } else { "disabled" },
        ready = false,
        message = "Workspace Relay host is listening with fail-closed authentication",
    );
    tracing::warn!(
        event.name = "platform.workspace_relay.production_gate",
        error.code = "RELAY_PRODUCTION_ACCESS_CONTROL_UNAVAILABLE",
        message = "Frontend identity, Directory administration, DeviceAuthorization, CA issuance, WebAuthn and Product private access are not connected; /readyz remains unavailable",
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

async fn serve_health<F>(listener: TcpListener, shutdown: F) -> io::Result<()>
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
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_health_request(stream).await {
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

async fn handle_health_request(mut stream: TcpStream) -> io::Result<()> {
    let request = timeout(HEALTH_REQUEST_TIMEOUT, read_health_request(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "health request timed out"))??;
    let request_line = request.lines().next().unwrap_or_default();
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();

    let (status, reason, body) = if method != "GET" || !version.starts_with("HTTP/1.") {
        (400, "Bad Request", r#"{"status":"bad_request"}"#)
    } else {
        match path {
            "/healthz" => (200, "OK", r#"{"status":"alive"}"#),
            "/readyz" => (503, "Service Unavailable", NOT_READY_BODY),
            _ => (404, "Not Found", r#"{"status":"not_found"}"#),
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

    fn loopback_bind() -> SocketAddr {
        "127.0.0.1:8080".parse().unwrap()
    }

    #[test]
    fn default_loopback_configuration_does_not_select_aca_identity() {
        assert_eq!(
            validate_aca_ingress_config(loopback_bind(), None, None).unwrap(),
            None
        );
    }

    #[test]
    fn aca_identity_requires_both_ingress_assertion_and_private_ca_bundle() {
        let ca_bundle = PathBuf::from("/etc/cyrene/device-ca/roots.pem");
        assert!(
            validate_aca_ingress_config(loopback_bind(), None, Some(ca_bundle.clone()))
                .unwrap_err()
                .to_string()
                .contains("ACA_INGRESS_ASSERTION")
        );
        assert!(
            validate_aca_ingress_config(loopback_bind(), Some(ACA_INGRESS_ASSERTION), None)
                .unwrap_err()
                .to_string()
                .contains(ACA_CLIENT_CA_BUNDLE_ENV)
        );
    }

    #[test]
    fn unknown_ingress_assertion_is_rejected() {
        assert!(validate_aca_ingress_config(
            loopback_bind(),
            Some("trust-forwarded-headers"),
            Some(PathBuf::from("/etc/cyrene/device-ca/roots.pem"))
        )
        .unwrap_err()
        .to_string()
        .contains(ACA_INGRESS_ASSERTION));
    }

    #[test]
    fn non_loopback_bind_requires_explicit_aca_identity_configuration() {
        let bind = "0.0.0.0:8080".parse().unwrap();
        assert!(validate_aca_ingress_config(bind, None, None)
            .unwrap_err()
            .to_string()
            .contains("ACA client CA bundle"));
        assert_eq!(
            validate_aca_ingress_config(
                bind,
                Some(ACA_INGRESS_ASSERTION),
                Some(PathBuf::from("/etc/cyrene/device-ca/roots.pem"))
            )
            .unwrap(),
            Some(PathBuf::from("/etc/cyrene/device-ca/roots.pem"))
        );
    }

    #[test]
    fn aca_ca_bundle_reader_enforces_its_size_limit() {
        let directory = tempfile::tempdir().unwrap();
        let bundle_path = directory.path().join("roots.pem");
        std::fs::write(&bundle_path, b"trusted roots").unwrap();
        assert_eq!(
            read_aca_client_ca_bundle(&bundle_path).unwrap(),
            b"trusted roots"
        );

        let oversized = vec![0_u8; usize::try_from(MAX_ACA_CLIENT_CA_BUNDLE_BYTES + 1).unwrap()];
        std::fs::write(&bundle_path, oversized).unwrap();
        assert!(read_aca_client_ca_bundle(&bundle_path).is_err());
    }
}
