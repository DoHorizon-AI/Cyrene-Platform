//! Linux integration fixture for a multi-binding CapabilityExecutionService.
//!
//! This is intentionally a small, generic process fixture rather than a
//! second registry or a product-specific connector host. It registers one
//! manifest and one stable `CapabilityBinding` per configured entry, then
//! supplies each binding's runtime environment to the existing worker
//! activator. Binding identity therefore remains independent from worker
//! process generation and executable details.

use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf, time::Duration};

use cy_capability_execution_service::{build_service, load_bindings, load_manifest, server};
use cy_platform_api::WorkerActivationOptions;
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse(env::args().skip(1))?;
    let manifest = load_manifest(&args.manifest)?;
    let binding_environments = load_bindings(&args.bindings)?;

    let base_worker_options = WorkerActivationOptions {
        working_dir: args.working_dir.clone(),
        python_path: args.python_path.clone(),
        python_executable: args.python_executable.clone(),
        environment: HashMap::new(),
        handshake_timeout: args.handshake_timeout,
        default_invoke_timeout: args.default_invoke_timeout,
        shutdown_grace_period: args.shutdown_grace_period,
        max_message_bytes: 1024 * 1024,
    };
    let service = build_service(
        manifest,
        base_worker_options,
        args.event_buffer_capacity,
        Some(binding_environments),
    )?;

    let service_for_shutdown = service.clone();
    let listener = TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    if let Some(ready_file) = args.ready_file.as_deref() {
        if let Some(parent) = ready_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(ready_file, format!("{address}\n"))?;
    }
    eprintln!("CapabilityExecutionService listening on {address}");

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    Server::builder()
        .add_service(server(service))
        .serve_with_incoming_shutdown(incoming, async move {
            shutdown_signal().await;
            service_for_shutdown.shutdown().await;
        })
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Debug)]
struct Arguments {
    bind: SocketAddr,
    manifest: PathBuf,
    bindings: PathBuf,
    ready_file: Option<PathBuf>,
    working_dir: Option<PathBuf>,
    python_path: Vec<PathBuf>,
    python_executable: Option<String>,
    handshake_timeout: Duration,
    default_invoke_timeout: Duration,
    shutdown_grace_period: Duration,
    event_buffer_capacity: usize,
}

impl Arguments {
    fn parse(values: impl Iterator<Item = String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut bind = "127.0.0.1:50051".parse()?;
        let mut manifest = None;
        let mut bindings = None;
        let mut ready_file = None;
        let mut working_dir = None;
        let mut python_path = Vec::new();
        let mut python_executable = None;
        let mut handshake_timeout = Duration::from_secs(5);
        let mut default_invoke_timeout = Duration::from_secs(30);
        let mut shutdown_grace_period = Duration::from_secs(2);
        let mut event_buffer_capacity = 32;
        let mut values = values;
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--bind" => bind = next_argument(&mut values, &argument)?.parse()?,
                "--manifest" => {
                    manifest = Some(PathBuf::from(next_argument(&mut values, &argument)?))
                }
                "--bindings" => {
                    bindings = Some(PathBuf::from(next_argument(&mut values, &argument)?))
                }
                "--ready-file" => {
                    ready_file = Some(PathBuf::from(next_argument(&mut values, &argument)?))
                }
                "--working-dir" => {
                    working_dir = Some(PathBuf::from(next_argument(&mut values, &argument)?))
                }
                "--python-path" => {
                    python_path.push(PathBuf::from(next_argument(&mut values, &argument)?))
                }
                "--python-executable" => {
                    python_executable = Some(next_argument(&mut values, &argument)?)
                }
                "--handshake-timeout-ms" => {
                    handshake_timeout =
                        Duration::from_millis(next_argument(&mut values, &argument)?.parse()?)
                }
                "--default-invoke-timeout-ms" => {
                    default_invoke_timeout =
                        Duration::from_millis(next_argument(&mut values, &argument)?.parse()?)
                }
                "--shutdown-grace-ms" => {
                    shutdown_grace_period =
                        Duration::from_millis(next_argument(&mut values, &argument)?.parse()?)
                }
                "--event-buffer-capacity" => {
                    event_buffer_capacity = next_argument(&mut values, &argument)?.parse()?
                }
                "--help" | "-h" => {
                    return Err("usage: configured_binding_server --manifest PATH --bindings PATH [--bind HOST:PORT] [--ready-file PATH] [--working-dir PATH] [--python-path PATH] [--python-executable PATH]".into())
                }
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }
        let manifest = manifest.ok_or("--manifest is required")?;
        let bindings = bindings.ok_or("--bindings is required")?;
        if !(1..=cy_platform_api::MAX_APPLICATION_EVENT_BUFFER_CAPACITY)
            .contains(&event_buffer_capacity)
        {
            return Err(format!(
                "--event-buffer-capacity must be between 1 and {}",
                cy_platform_api::MAX_APPLICATION_EVENT_BUFFER_CAPACITY
            )
            .into());
        }
        Ok(Self {
            bind,
            manifest,
            bindings,
            ready_file,
            working_dir,
            python_path,
            python_executable,
            handshake_timeout,
            default_invoke_timeout,
            shutdown_grace_period,
            event_buffer_capacity,
        })
    }
}

fn next_argument(
    values: &mut impl Iterator<Item = String>,
    argument: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    values
        .next()
        .ok_or_else(|| format!("{argument} requires a value").into())
}
