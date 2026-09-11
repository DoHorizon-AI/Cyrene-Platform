// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: runtime/cyrene-kernel/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Linux composition root for the CYRENE Kernel daemon.
//!
//! The process owns no vendor driver code. It joins versioned UDS system and
//! hardware adapters plus one separately supervised Sandbox Adapter Host, then
//! serves Core v1 over a local UDS endpoint. Admission is enforced by
//! Unix-socket peer credentials (SO_PEERCRED): every Adapter endpoint must be
//! configured with a trusted peer UID/GID (fail-closed at startup), and the
//! authority socket also authenticates the calling Principal from the peer
//! credential.

mod runtime_journal;

#[cfg(not(unix))]
fn main() {
    eprintln!("cyrene-kernel is supported only on Linux/Unix cgroup hosts");
    std::process::exit(1);
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use cy_installation_resolver::FilesystemInstalledPluginResolver;
    use cy_kernel_daemon::{
        peer_cred::{inject_authority_principal, PeerCredAccept},
        KernelDaemon, KernelServiceAdapter, WorkerHeartbeatConfig,
    };
    use cy_proto::{core_v1, core_v2, provider_v1};
    use cy_resource_manager::InMemoryResourceManager;
    use cy_sandbox_client::UdsSandboxAdapterClient;
    use runtime_journal::FileRuntimeJournal;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::Server;

    let args = Args::parse()?;
    // Persist and advance the epoch before accepting any Kernel request. The
    // journal is evidence only: it seeds fencing but is never used to adopt a
    // Worker left by an older sandboxd process.
    let journal = Arc::new(FileRuntimeJournal::open(&args.runtime_journal)?);
    let recovery = journal.begin_epoch(&args.node_id)?;
    if !recovery.runtime_processes.is_empty() {
        eprintln!(
            "recovery discovered {} unclosed runtime record(s); they remain fenced and require provider reconciliation",
            recovery.runtime_processes.len()
        );
    }
    let sandbox = Arc::new(UdsSandboxAdapterClient::from_endpoint(
        args.sandbox_adapter,
    )?);
    // Do not bind any public or worker listener until sandboxd has classified
    // every leftover process against the local journal and reaped only exact,
    // stale Kernel evidence. Unknown and foreign state fail startup closed.
    journal
        .recover_before_listeners(&args.node_id, &recovery, sandbox.as_ref())
        .map_err(|error| std::io::Error::other(format!("restart recovery failed: {error}")))?;

    let resources = Arc::new(InMemoryResourceManager::with_next_fence_token(
        &args.node_id,
        Vec::new(),
        recovery.next_fence_token,
    ));
    let daemon = Arc::new(KernelDaemon::with_adapters(
        args.adapters,
        resources,
        sandbox,
        &args.node_id,
        recovery.node_epoch,
    )?);
    if !daemon.sandbox_preflight_ready() {
        return Err(std::io::Error::other(
            "sandbox adapter preflight did not meet Kernel capabilities",
        )
        .into());
    }
    let heartbeat = WorkerHeartbeatConfig {
        socket_path: args.worker_control_socket.clone(),
        interval: args.heartbeat_interval,
        timeout: args.heartbeat_timeout,
        graceful_stop: args.heartbeat_grace,
        shutdown_ack_timeout: args.shutdown_ack_timeout,
    };
    let adapter = KernelServiceAdapter::new(
        daemon,
        Arc::new(
            FilesystemInstalledPluginResolver::new(args.installations_root)
                .with_worker_transport_root(args.worker_transport_root.clone()),
        ),
    )
    .with_worker_heartbeat(heartbeat)
    .with_adapter_poll_interval(args.adapter_poll_interval)
    .with_runtime_journal(journal.clone())
    .with_event_store(journal);
    // The Adapter owns the hardware Provider lifecycle. Sync before accepting
    // listeners so every configured adapter has a resource-only Provider view.
    adapter.sync_hardware_provider_facts().map_err(|error| {
        std::io::Error::other(format!(
            "failed to synchronize hardware provider facts: {error}"
        ))
    })?;
    let _watchdog = adapter.start_watchdog();
    let _adapter_monitor = adapter.start_adapter_monitor();

    let authority_listener = bind_socket(&args.socket)?;
    let worker_control_listener = bind_socket(&args.worker_control_socket)?;
    let provider_listener = bind_socket(&args.provider_socket)?;
    let authority_server = Server::builder()
        .add_service(
            core_v1::kernel_authority_service_server::KernelAuthorityServiceServer::with_interceptor(
                adapter.clone(),
                inject_authority_principal,
            ),
        )
        .add_service(
            core_v2::kernel_authority_service_server::KernelAuthorityServiceServer::with_interceptor(
                adapter.clone(),
                inject_authority_principal,
            ),
        )
        .add_service(adapter.server())
        .serve_with_incoming(PeerCredAccept::new(UnixListenerStream::new(
            authority_listener,
        )));
    // The Worker socket exposes only the constrained control/liveness service.
    // It never registers KernelAuthorityService, so a Worker cannot call lease
    // or Endpoint authority actions merely because it can acknowledge shutdown.
    let worker_control_server = Server::builder()
        .add_service(adapter.worker_control_server())
        .add_service(adapter.lifecycle_server())
        .serve_with_incoming(UnixListenerStream::new(worker_control_listener));
    let provider_server = Server::builder()
        .add_service(
            provider_v1::kernel_provider_service_server::KernelProviderServiceServer::with_interceptor(
                adapter.clone(),
                inject_authority_principal,
            ),
        )
        .serve_with_incoming(PeerCredAccept::new(UnixListenerStream::new(
            provider_listener,
        )));
    tokio::try_join!(authority_server, worker_control_server, provider_server)?;
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct Args {
    node_id: String,
    socket: std::path::PathBuf,
    worker_control_socket: std::path::PathBuf,
    provider_socket: std::path::PathBuf,
    adapters: Vec<cy_adapter_client::HardwareAdapterEndpoint>,
    sandbox_adapter: cy_sandbox_client::SandboxAdapterEndpoint,
    installations_root: std::path::PathBuf,
    worker_transport_root: std::path::PathBuf,
    runtime_journal: std::path::PathBuf,
    heartbeat_interval: std::time::Duration,
    heartbeat_timeout: std::time::Duration,
    heartbeat_grace: std::time::Duration,
    shutdown_ack_timeout: std::time::Duration,
    adapter_poll_interval: std::time::Duration,
}

#[cfg(unix)]
impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        use std::{collections::BTreeMap, env, path::PathBuf, time::Duration};
        let mut values = env::args().skip(1);
        let mut node_id = env::var("CYRENE_NODE_ID").unwrap_or_else(|_| "cyrene-node".to_string());
        let mut socket = PathBuf::from("/run/cyrene/kernel.sock");
        let mut worker_control_socket = PathBuf::from("/run/cyrene/worker.sock");
        let mut provider_socket = PathBuf::from("/run/cyrene/provider.sock");
        let mut system_adapters = Vec::new();
        let mut hardware_adapters = Vec::new();
        let mut sandbox_adapter = None;
        let mut installations_root = PathBuf::from("/var/lib/cyrene/installations");
        let mut worker_transport_root = PathBuf::from("/run/cyrene/workers");
        let mut runtime_journal = PathBuf::from("/var/lib/cyrene/runtime/journal.jsonl");
        let mut heartbeat_interval = Duration::from_secs(5);
        let mut heartbeat_timeout = Duration::from_secs(20);
        let mut heartbeat_grace = Duration::from_secs(10);
        let mut shutdown_ack_timeout = Duration::from_secs(3);
        let mut adapter_poll_interval = Duration::from_secs(5);
        let mut adapter_peer_credentials =
            BTreeMap::<String, cy_adapter_client::PeerCredentialExpectation>::new();
        let mut sandbox_peer_uid = None;
        let mut sandbox_peer_gid = None;
        while let Some(argument) = values.next() {
            let mut value = || {
                values.next().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{argument} requires a value"),
                    )
                })
            };
            match argument.as_str() {
                "--node-id" => node_id = value()?,
                "--socket" => socket = PathBuf::from(value()?),
                "--worker-control-socket" => worker_control_socket = PathBuf::from(value()?),
                "--provider-socket" => provider_socket = PathBuf::from(value()?),
                "--system-adapter" => system_adapters.push(parse_hardware_adapter(&value()?)?),
                "--hardware-adapter" => hardware_adapters.push(parse_hardware_adapter(&value()?)?),
                "--system-adapter-peer-uid" | "--hardware-adapter-peer-uid" => {
                    let (adapter_id, uid) = parse_adapter_identity_value(&value()?)?;
                    adapter_peer_credentials.entry(adapter_id).or_default().uid = Some(uid);
                }
                "--system-adapter-peer-gid" | "--hardware-adapter-peer-gid" => {
                    let (adapter_id, gid) = parse_adapter_identity_value(&value()?)?;
                    adapter_peer_credentials.entry(adapter_id).or_default().gid = Some(gid);
                }
                "--sandbox-adapter" => sandbox_adapter = Some(parse_sandbox_adapter(&value()?)?),
                "--sandbox-adapter-peer-uid" => {
                    sandbox_peer_uid = Some(value()?.parse::<u32>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--sandbox-adapter-peer-uid must be an unsigned integer",
                        )
                    })?)
                }
                "--sandbox-adapter-peer-gid" => {
                    sandbox_peer_gid = Some(value()?.parse::<u32>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--sandbox-adapter-peer-gid must be an unsigned integer",
                        )
                    })?)
                }
                "--installations-root" => installations_root = PathBuf::from(value()?),
                "--worker-transport-root" => worker_transport_root = PathBuf::from(value()?),
                "--runtime-journal" => runtime_journal = PathBuf::from(value()?),
                "--heartbeat-interval-ms" => heartbeat_interval = Duration::from_millis(value()?.parse()?),
                "--heartbeat-timeout-ms" => heartbeat_timeout = Duration::from_millis(value()?.parse()?),
                "--heartbeat-grace-ms" => heartbeat_grace = Duration::from_millis(value()?.parse()?),
                "--shutdown-ack-timeout-ms" => shutdown_ack_timeout = Duration::from_millis(value()?.parse()?),
                "--adapter-poll-interval-ms" => adapter_poll_interval = Duration::from_millis(value()?.parse()?),
                "--help" | "-h" => return Err(std::io::Error::other("usage: cyrene-kernel --system-adapter ID=/absolute/socket.sock [--system-adapter-peer-uid ID=UID] [--system-adapter-peer-gid ID=GID] [--hardware-adapter ID=/absolute/socket.sock] [--hardware-adapter-peer-uid ID=UID] [--hardware-adapter-peer-gid ID=GID] --sandbox-adapter ID=/absolute/socket.sock [--sandbox-adapter-peer-uid UID] [--sandbox-adapter-peer-gid GID] [--node-id ID] [--socket AUTHORITY_PATH] [--worker-control-socket PATH] [--provider-socket PATH] [--installations-root PATH] [--runtime-journal PATH] [--heartbeat-interval-ms N] [--heartbeat-timeout-ms N] [--heartbeat-grace-ms N] [--shutdown-ack-timeout-ms N] [--adapter-poll-interval-ms N]").into()),
                _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("unknown argument: {argument}")).into()),
            }
        }
        if heartbeat_interval.is_zero() || heartbeat_timeout <= heartbeat_interval {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "heartbeat timeout must be greater than a non-zero heartbeat interval",
            )
            .into());
        }
        if shutdown_ack_timeout.is_zero() || adapter_poll_interval.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shutdown acknowledgement timeout and adapter polling interval must be non-zero",
            )
            .into());
        }
        if system_adapters.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "at least one --system-adapter ID=/absolute/socket.sock is required",
            )
            .into());
        }
        if !socket.is_absolute()
            || !worker_control_socket.is_absolute()
            || !provider_socket.is_absolute()
            || socket == worker_control_socket
            || socket == provider_socket
            || worker_control_socket == provider_socket
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "authority, Worker-control, and Provider socket paths must be distinct and absolute",
            )
                .into());
        }
        let mut adapters = system_adapters;
        adapters.extend(hardware_adapters);
        for endpoint in &mut adapters {
            if let Some(credentials) = adapter_peer_credentials.remove(&endpoint.adapter_id) {
                endpoint.peer_credentials = credentials;
            }
        }
        if let Some(adapter_id) = adapter_peer_credentials.keys().next() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("peer credential policy references unknown adapter: {adapter_id}"),
            )
            .into());
        }
        let mut sandbox_adapter = sandbox_adapter.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "exactly one --sandbox-adapter ID=/absolute/socket.sock is required",
            )
        })?;
        // The single sandbox adapter has no routing ID ambiguity, so its peer
        // identity policy is configured as a bare UID/GID rather than ID=NUMBER.
        sandbox_adapter.peer_credentials = cy_adapter_client::PeerCredentialExpectation {
            uid: sandbox_peer_uid,
            gid: sandbox_peer_gid,
        };
        // Fail-closed admission gate: every Adapter endpoint must be configured
        // with at least one trusted peer UID/GID. Without this, the library
        // default of `PeerCredentialExpectation::default()` (allow any peer)
        // would silently disable UDS admission for that endpoint in production.
        require_configured_adapter_peers(&adapters, &sandbox_adapter)?;
        Ok(Self {
            node_id,
            socket,
            worker_control_socket,
            provider_socket,
            adapters,
            sandbox_adapter,
            installations_root,
            worker_transport_root,
            runtime_journal,
            heartbeat_interval,
            heartbeat_timeout,
            heartbeat_grace,
            shutdown_ack_timeout,
            adapter_poll_interval,
        })
    }
}

#[cfg(unix)]
fn require_configured_adapter_peers(
    adapters: &[cy_adapter_client::HardwareAdapterEndpoint],
    sandbox_adapter: &cy_sandbox_client::SandboxAdapterEndpoint,
) -> std::io::Result<()> {
    for endpoint in adapters {
        if !endpoint.peer_credentials.is_configured() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "adapter '{}' requires at least one system/hardware adapter peer UID or GID",
                    endpoint.adapter_id
                ),
            ));
        }
    }
    if !sandbox_adapter.peer_credentials.is_configured() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sandbox adapter requires at least one --sandbox-adapter-peer-uid or --sandbox-adapter-peer-gid",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn bind_socket(path: &std::path::Path) -> Result<tokio::net::UnixListener, std::io::Error> {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener as StdUnixListener},
    };
    prepare_socket_path(path)?;
    let listener = StdUnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    tokio::net::UnixListener::from_std(listener)
}

#[cfg(unix)]
fn parse_adapter_identity_value(value: &str) -> Result<(String, u32), std::io::Error> {
    let (adapter_id, identity) = value.split_once('=').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "adapter peer identity must use ID=NUMBER",
        )
    })?;
    if adapter_id.is_empty()
        || !adapter_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "adapter peer identity ID must be safe",
        ));
    }
    Ok((
        adapter_id.to_string(),
        identity.parse::<u32>().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "adapter peer identity must be an unsigned integer",
            )
        })?,
    ))
}

#[cfg(unix)]
fn parse_sandbox_adapter(
    value: &str,
) -> Result<cy_sandbox_client::SandboxAdapterEndpoint, std::io::Error> {
    let (adapter_id, socket_path) = value.split_once('=').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sandbox adapter must use ID=/absolute/socket.sock",
        )
    })?;
    let socket_path = std::path::PathBuf::from(socket_path);
    if adapter_id.is_empty()
        || !adapter_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        || !socket_path.is_absolute()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sandbox adapter ID must be safe and its UDS socket path absolute",
        ));
    }
    Ok(cy_sandbox_client::SandboxAdapterEndpoint::new(
        adapter_id,
        socket_path,
    ))
}

#[cfg(unix)]
fn parse_hardware_adapter(
    value: &str,
) -> Result<cy_adapter_client::HardwareAdapterEndpoint, std::io::Error> {
    let (adapter_id, socket_path) = value.split_once('=').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hardware adapter must use ID=/absolute/socket.sock",
        )
    })?;
    let socket_path = std::path::PathBuf::from(socket_path);
    if adapter_id.is_empty()
        || !adapter_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        || !socket_path.is_absolute()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hardware adapter ID must be safe and its UDS socket path absolute",
        ));
    }
    Ok(cy_adapter_client::HardwareAdapterEndpoint::new(
        adapter_id,
        socket_path,
    ))
}

#[cfg(unix)]
fn prepare_socket_path(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::{fs, os::unix::fs::FileTypeExt};
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "kernel socket needs a parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    if !path.exists() {
        return Ok(());
    }
    let kind = fs::symlink_metadata(path)?.file_type();
    if !kind.is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to replace a non-socket path: {}", path.display()),
        ));
    }
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::require_configured_adapter_peers;
    use cy_adapter_client::{HardwareAdapterEndpoint, PeerCredentialExpectation};
    use cy_sandbox_client::SandboxAdapterEndpoint;
    use std::path::PathBuf;

    fn hardware(id: &str) -> HardwareAdapterEndpoint {
        HardwareAdapterEndpoint::new(id, PathBuf::from("/run/cyrene/test-hardware.sock"))
    }

    fn sandbox() -> SandboxAdapterEndpoint {
        SandboxAdapterEndpoint::new("sandboxd", PathBuf::from("/run/cyrene/test-sandbox.sock"))
    }

    #[test]
    fn gate_rejects_unconfigured_hardware_adapter() {
        let hw = hardware("nvidia");
        let sb = sandbox();
        assert!(require_configured_adapter_peers(&[hw], &sb).is_err());
    }

    #[test]
    fn gate_rejects_unconfigured_sandbox_adapter() {
        let mut hw = hardware("nvidia");
        hw.peer_credentials = PeerCredentialExpectation {
            uid: Some(0),
            gid: None,
        };
        let sb = sandbox();
        assert!(require_configured_adapter_peers(&[hw], &sb).is_err());
    }

    #[test]
    fn gate_accepts_fully_configured_endpoints() {
        let mut hw = hardware("nvidia");
        hw.peer_credentials = PeerCredentialExpectation {
            uid: Some(0),
            gid: None,
        };
        let mut sb = sandbox();
        sb.peer_credentials = PeerCredentialExpectation {
            uid: Some(0),
            gid: None,
        };
        assert!(require_configured_adapter_peers(&[hw], &sb).is_ok());
    }
}
