//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/main.rs            ║
//! ║ Module: cy_workspace_web_bff                                       ║
//! ║ Role: Start the Web BFF health listener.                           ║
//! ║                                                                    ║
//! ║ 模块职责：启动 Web BFF 健康探针 listener。                          ║
//! ╚══════════════════════════════════════════════════════════════════════╝

use std::env;
use std::net::SocketAddr;

use tokio::net::TcpListener;

mod host;

use host::HostStartupError;

const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8080";
const LISTEN_ADDRESS_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_BIND";

/// Composes production providers before serving the authenticated BFF router.
///
/// Any missing or unavailable startup dependency terminates startup before the listener opens.
/// ACA must use internal ingress; this process does not establish a public network boundary.
///
/// 先装配真实生产 provider，再启动已认证 BFF router。启动依赖缺失或不可用时，不打开 listener。
async fn run() -> Result<(), HostStartupError> {
    let application = host::compose().await?;
    let listen_address = env::var(LISTEN_ADDRESS_ENV)
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDRESS.to_string())
        .parse::<SocketAddr>()
        .map_err(|_| HostStartupError::Configuration)?;
    let listener = TcpListener::bind(listen_address).await?;
    axum::serve(listener, application).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), HostStartupError> {
    run().await
}
