//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connector_authorization_main.rs                                 │
//! │  Module: cy_workspace_connector_authority_host                     │
//! │  Role: Start the connector-authorization-only production profile.   │
//! │                                                                     │
//! │  模块职责：启动仅 Connector 授权 RPC 的生产 profile。                 │
//! └─────────────────────────────────────────────────────────────────────┘

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = cy_workspace_authority_host::run_connector_authorization_host().await {
        tracing::error!(%error, "Workspace connector authorization Authority failed to start or stopped");
        std::process::exit(1);
    }
}
