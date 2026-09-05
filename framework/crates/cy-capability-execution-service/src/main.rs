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

use std::{env, fs, net::SocketAddr, path::PathBuf, time::Duration};

use cy_capability_execution_service::{build_service, load_bindings, load_manifest, server};
use cy_platform_api::WorkerActivationOptions;
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse(env::args().skip(1))?;
    let manifest = load_manifest(&args.manifest)?;
    let configured_bindings = args.bindings.as_deref().map(load_bindings).transpose()?;

    let base_worker_options = WorkerActivationOptions {
        working_dir: args.working_dir,
        python_path: args.python_path,
        python_executable: args.python_executable,
        environment: Default::default(),
        handshake_timeout: args.handshake_timeout,
        default_invoke_timeout: args.default_invoke_timeout,
        shutdown_grace_period: args.shutdown_grace_period,
        max_message_bytes: 1024 * 1024,
    };
    let service = build_service(
        manifest,
        base_worker_options,
        args.event_buffer_capacity,
        configured_bindings,
    )?;
    let service_for_shutdown = service.clone();
    let listener = TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    let _ready_file = ReadyFile::publish(args.ready_file.as_deref(), address)?;
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

#[derive(Debug)]
struct Arguments {
    bind: SocketAddr,
    manifest: PathBuf,
    bindings: Option<PathBuf>,
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
        let mut bind: SocketAddr = "127.0.0.1:50051".parse()?;
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
                    return Err("usage: cyrene-capability-execution-service --manifest PATH [--bindings PATH] [--bind LOOPBACK:PORT] [--ready-file PATH] [--working-dir PATH] [--python-path PATH] [--python-executable PATH] [--event-buffer-capacity N]".into())
                }
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }
        let manifest = manifest.ok_or("--manifest is required")?;
        if !bind.ip().is_loopback() {
            return Err("--bind must use a loopback address because CES TCP has no transport authentication".into());
        }
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

/// Removes the endpoint discovery file when the process exits gracefully.
struct ReadyFile(Option<PathBuf>);

impl ReadyFile {
    fn publish(path: Option<&std::path::Path>, address: SocketAddr) -> std::io::Result<Self> {
        let Some(path) = path else {
            return Ok(Self(None));
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, format!("{address}\n"))?;
        Ok(Self(Some(path.to_path_buf())))
    }
}

impl Drop for ReadyFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.as_deref() {
            let _ = fs::remove_file(path);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Arguments {
        Arguments::parse(arguments.iter().map(|argument| (*argument).to_string()))
            .expect("valid service arguments")
    }

    #[test]
    fn bindings_are_optional_for_legacy_single_manifest_mode() {
        let arguments = parse(&["--manifest", "manifest.json"]);

        assert_eq!(arguments.manifest, PathBuf::from("manifest.json"));
        assert_eq!(arguments.bindings, None);
    }

    #[test]
    fn bindings_path_is_parsed_without_changing_manifest_selection() {
        let arguments = parse(&[
            "--manifest",
            "manifest.json",
            "--bindings",
            "bindings.json",
            "--ready-file",
            "ready.txt",
        ]);

        assert_eq!(arguments.manifest, PathBuf::from("manifest.json"));
        assert_eq!(arguments.bindings, Some(PathBuf::from("bindings.json")));
        assert_eq!(arguments.ready_file, Some(PathBuf::from("ready.txt")));
    }

    #[test]
    fn refuses_unauthenticated_non_loopback_tcp() {
        let error = Arguments::parse(
            ["--manifest", "manifest.json", "--bind", "0.0.0.0:50051"]
                .into_iter()
                .map(str::to_string),
        )
        .expect_err("unauthenticated public CES listener must fail closed");

        assert!(error.to_string().contains("loopback"));
    }
}
