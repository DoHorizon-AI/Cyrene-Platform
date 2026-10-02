#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = cy_workspace_authority_host::run_host().await {
        tracing::error!(%error, "Workspace Authority failed to start or stopped");
        std::process::exit(1);
    }
}
