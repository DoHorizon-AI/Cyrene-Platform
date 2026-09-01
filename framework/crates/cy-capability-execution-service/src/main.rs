// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-capability-execution-service/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Platform capability execution service process.
//!
//! The binary deliberately accepts one canonical manifest path and generic
//! worker runtime settings. It has no media, connector, or Product-specific
//! command surface. ServiceSupervisor is expected to own this process's
//! restart/backoff policy in a deployed node.

use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use cy_capability_execution_service::{
    CapabilityExecutionConfig, CapabilityExecutionService, server,
};
use cy_manifest::PluginManifest;
use cy_platform_api::normalize_official_manifest;
use cy_platform_api::{CapabilityRegistry, WorkerActivationOptions};
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse(env::args().skip(1))?;
    let manifest = load_manifest(&args.manifest)?;
    let mut registry = CapabilityRegistry::new();
    registry.register(manifest.clone())?;

    let worker_options = WorkerActivationOptions {
        working_dir: args.working_dir,
        python_path: args.python_path,
        python_executable: args.python_executable,
        environment: HashMap::new(),
        handshake_timeout: args.handshake_timeout,
        default_invoke_timeout: args.default_invoke_timeout,
        shutdown_grace_period: args.shutdown_grace_period,
        max_message_bytes: 1024 * 1024,
    };
    let config = CapabilityExecutionConfig {
        worker_options: worker_options.clone(),
        application_event_buffer_capacity: args.event_buffer_capacity,
        ..CapabilityExecutionConfig::default()
    };
    let service = CapabilityExecutionService::new(registry, config).with_provider_worker_options(
        manifest.plugin.id.clone(),
        manifest.plugin.version.clone(),
        worker_options,
    );
    let service_for_shutdown = service.clone();
    let listener = TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    eprintln!("CapabilityExecutionService listening on {address}");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    Server::builder()
        .add_service(server(service))
        .serve_with_incoming_shutdown(incoming, async move {
            let _ = tokio::signal::ctrl_c().await;
            service_for_shutdown.shutdown().await;
        })
        .await?;
    Ok(())
}

fn load_manifest(path: &Path) -> Result<PluginManifest, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    if value.get("plugin").is_some() {
        Ok(serde_json::from_value(value)?)
    } else {
        Ok(normalize_official_manifest(value)?)
    }
}

#[derive(Debug)]
struct Arguments {
    bind: SocketAddr,
    manifest: PathBuf,
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
                    return Err("usage: cyrene-capability-execution-service --manifest PATH [--bind HOST:PORT] [--working-dir PATH] [--python-path PATH] [--python-executable PATH] [--event-buffer-capacity N]".into())
                }
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }
        let manifest = manifest.ok_or("--manifest is required")?;
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
