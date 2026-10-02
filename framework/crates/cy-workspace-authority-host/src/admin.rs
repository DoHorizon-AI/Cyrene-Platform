#[tokio::main]
async fn main() {
    if let Err(error) = cy_workspace_authority_host::run_admin_cli().await {
        eprintln!("Authority admin request failed: {error}");
        std::process::exit(1);
    }
}
