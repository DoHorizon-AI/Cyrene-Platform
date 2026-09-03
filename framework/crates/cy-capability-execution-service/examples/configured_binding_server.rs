//! Linux integration fixture for a multi-binding CapabilityExecutionService.
//!
//! This is intentionally a small, generic process fixture rather than a
//! second registry or a product-specific connector host. It registers one
//! manifest and one stable `CapabilityBinding` per configured entry, then
//! supplies each binding's runtime environment to the existing worker
//! activator. Binding identity therefore remains independent from worker
//! process generation and executable details.

use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use cy_capability_execution_service::{
    CapabilityExecutionConfig, CapabilityExecutionService, server,
};
use cy_manifest::PluginManifest;
use cy_platform_api::{
    CapabilityBinding, CapabilityRegistry, WorkerActivationOptions, normalize_official_manifest,
};
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse(env::args().skip(1))?;
    let manifest = load_manifest(&args.manifest)?;
    let binding_environments = load_bindings(&args.bindings)?;

    let mut registry = CapabilityRegistry::new();
    registry.register(manifest.clone())?;
    for binding in &binding_environments {
        registry.register_binding(CapabilityBinding::new(
            binding.id.clone(),
            manifest.plugin.id.clone(),
            manifest.plugin.version.clone(),
        )?)?;
    }

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
    let config = CapabilityExecutionConfig {
        worker_options: base_worker_options,
        application_event_buffer_capacity: args.event_buffer_capacity,
        ..CapabilityExecutionConfig::default()
    };
    let mut service = CapabilityExecutionService::new(registry, config);
    for binding in binding_environments {
        let options = WorkerActivationOptions {
            environment: binding.environment,
            working_dir: args.working_dir.clone(),
            python_path: args.python_path.clone(),
            python_executable: args.python_executable.clone(),
            handshake_timeout: args.handshake_timeout,
            default_invoke_timeout: args.default_invoke_timeout,
            shutdown_grace_period: args.shutdown_grace_period,
            max_message_bytes: 1024 * 1024,
        };
        service = service.with_binding_worker_options(binding.id, options);
    }

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

fn load_manifest(path: &Path) -> Result<PluginManifest, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    if value.get("plugin").is_some() {
        Ok(serde_json::from_value(value)?)
    } else {
        Ok(normalize_official_manifest(value)?)
    }
}

#[derive(Debug)]
struct BindingEnvironment {
    id: String,
    environment: HashMap<String, String>,
}

fn load_bindings(path: &Path) -> Result<Vec<BindingEnvironment>, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let entries = value
        .as_array()
        .ok_or("--bindings must contain a JSON array")?;
    let mut bindings = Vec::with_capacity(entries.len());
    let mut ids = std::collections::HashSet::with_capacity(entries.len());
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or("each configured binding must be a JSON object")?;
        let id = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or("each configured binding requires a non-empty id")?
            .to_string();
        if !ids.insert(id.clone()) {
            return Err(format!("duplicate configured binding id: {id}").into());
        }
        let environment = object
            .get("environment")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("binding {id} requires an environment object"))?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| format!("binding {id} environment value {key} must be a string"))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        bindings.push(BindingEnvironment { id, environment });
    }
    if bindings.is_empty() {
        return Err("--bindings must contain at least one configured binding".into());
    }
    Ok(bindings)
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
