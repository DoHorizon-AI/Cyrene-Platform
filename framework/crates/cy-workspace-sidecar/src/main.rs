//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-sidecar.rs                                         │
//! │  Module: cy_workspace_sidecar                                       │
//! │  Role: Loopback-only Workspace API proxy for Python clients.         │
//! │                                                                     │
//! │  模块职责：为 Python client 提供仅 loopback 可访问的 Workspace API proxy。 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::env;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use cy_proto::workspace_local_v1::workspace_sidecar_service_server::WorkspaceSidecarServiceServer;
use cy_workspace_sidecar::{LocalBearerInterceptor, WorkspaceSidecar};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;

const DEFAULT_PORT: u16 = 41_680;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let credential_bundle_path = required_path("CYRENE_WORKSPACE_SIDECAR_CREDENTIAL_BUNDLE")?;
    let local_token_path = required_path("CYRENE_WORKSPACE_SIDECAR_LOCAL_TOKEN_FILE")?;
    let port = env::var("CYRENE_WORKSPACE_SIDECAR_PORT")
        .ok()
        .map(|value| value.parse::<u16>())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    if port == 0 {
        return Err("CYRENE_WORKSPACE_SIDECAR_PORT must be non-zero".into());
    }

    let interceptor = LocalBearerInterceptor::from_file(&local_token_path)?;
    let sidecar = WorkspaceSidecar::new(credential_bundle_path, &interceptor)?;
    let service = WorkspaceSidecarServiceServer::new(sidecar)
        .max_decoding_message_size(1024 * 1024)
        .max_encoding_message_size(1024 * 1024);
    let service = InterceptedService::new(service, interceptor);
    let bind = loopback_address(port);
    eprintln!("cy-workspace-sidecar listening on {bind}");
    Server::builder().add_service(service).serve(bind).await?;
    Ok(())
}

fn loopback_address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn required_path(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::var_os(name).ok_or_else(|| format!("{name} is required"))?;
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_address_is_ipv4_loopback_only() {
        let address = loopback_address(DEFAULT_PORT);
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(address.port(), DEFAULT_PORT);
    }
}
