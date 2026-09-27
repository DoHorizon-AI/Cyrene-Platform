//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/main.rs            ║
//! ║ Module: cy_workspace_web_bff                                       ║
//! ║ Role: Start the Web BFF health listener.                           ║
//! ║                                                                    ║
//! ║ 模块职责：启动 Web BFF 健康探针 listener。                          ║
//! ╚══════════════════════════════════════════════════════════════════════╝

use tokio::net::TcpListener;

mod host;

use host::HostStartupError;

const LISTEN_ADDRESS: &str = "0.0.0.0:8080";

/// Composes production providers before serving the authenticated BFF router.
///
/// Any missing or unavailable startup dependency terminates startup before the listener opens.
/// ACA must use internal ingress; this process does not establish a public network boundary.
///
/// 先装配真实生产 provider，再启动已认证 BFF router。启动依赖缺失或不可用时，不打开 listener。
async fn run() -> Result<(), HostStartupError> {
    let application = host::compose().await?;
    let listener = TcpListener::bind(LISTEN_ADDRESS).await?;
    axum::serve(listener, application).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), HostStartupError> {
    run().await
}
