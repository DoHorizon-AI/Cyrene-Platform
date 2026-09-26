//! ╔══════════════════════════════════════════════════════════════════════╗
//! ║ File: framework/crates/cy-workspace-web-bff/src/main.rs            ║
//! ║ Module: cy_workspace_web_bff                                       ║
//! ║ Role: Start the Web BFF health listener.                           ║
//! ║                                                                    ║
//! ║ 模块职责：启动 Web BFF 健康探针 listener。                          ║
//! ╚══════════════════════════════════════════════════════════════════════╝

mod host;

use tokio::net::TcpListener;

const LISTEN_ADDRESS: &str = "0.0.0.0:8080";

/// Runs the BFF health listener on the container's fixed internal target port.
///
/// The first host slice intentionally remains unready until real providers are composed.
/// ACA must use internal ingress; this process does not establish a public network boundary.
///
/// 在容器固定内部端口启动 BFF 健康 listener。真实 provider 装配完成前保持未就绪；ACA 必须使用 internal ingress。
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(LISTEN_ADDRESS).await?;
    axum::serve(listener, host::health_router()).await
}
