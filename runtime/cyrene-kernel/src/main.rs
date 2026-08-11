//! Linux composition root for the CYRENE Kernel daemon.
//!
//! The process owns no vendor driver code. It joins a versioned UDS hardware
//! adapter and one separately supervised Sandbox Adapter Host, then serves Core
//! v1 over a local UDS endpoint. Admission is enforced by Unix-socket peer
//! credentials (SO_PEERCRED): every Adapter endpoint must be configured with a
//! trusted peer UID/GID (fail-closed at startup), and the authority socket also
//! authenticates the calling Principal from the peer credential.

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
    use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter, WorkerHeartbeatConfig};
    use cy_resource_manager::InMemoryResourceManager;
    use cy_sandbox_client::UdsSandboxAdapterClient;
    use runtime_journal::FileRuntimeJournal;
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::Server;

    let args = Args::parse()?;
    // Persist and advance the epoch before accepting any Kernel request. The
    // journal is evidence only: it seeds fencing but is never used to adopt a
    // Worker left by an older sandboxd process.
    let journal = Arc::new(FileRuntimeJournal::open(&args.runtime_journal)?);
    let recovery = journal.begin_epoch(&args.node_id)?;
    let sandbox = Arc::new(UdsSandboxAdapterClient::from_endpoint(
        args.sandbox_adapter,
    )?);

    let resources = Arc::new(InMemoryResourceManager::with_next_fence_token(
        &args.node_id,
        Vec::new(),
        recovery.next_fence_token,
    ));
    let daemon = Arc::new(KernelDaemon::with_hardware_adapters(
        args.hardware_adapters,
        resources,
        sandbox,
        &args.node_id,
        recovery.node_epoch,
    )?);
    if !daemon.preflight_ready() {
        return Err(std::io::Error::other(
            "required hardware or sandbox adapter preflight did not meet Kernel capabilities",
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
        Arc::new(FilesystemInstalledPluginResolver::new(
            args.installations_root,
        )),
    )
    .with_worker_heartbeat(heartbeat)
    .with_adapter_poll_interval(args.adapter_poll_interval)
    .with_runtime_journal(journal);
    let _watchdog = adapter.start_watchdog();
    let _adapter_monitor = adapter.start_adapter_monitor();

    let authority_listener = bind_socket(&args.socket)?;
    let worker_control_listener = bind_socket(&args.worker_control_socket)?;
    let authority_server = Server::builder()
        .add_service(adapter.authority_server())
        .add_service(adapter.server())
        .serve_with_incoming(UnixListenerStream::new(authority_listener));
    // The Worker socket exposes only the constrained control/liveness service.
    // It never registers KernelAuthorityService, so a Worker cannot call lease
    // or Endpoint authority actions merely because it can acknowledge shutdown.
    let worker_control_server = Server::builder()
        .add_service(adapter.worker_control_server())
        .add_service(adapter.lifecycle_server())
        .serve_with_incoming(UnixListenerStream::new(worker_control_listener));
    tokio::try_join!(authority_server, worker_control_server)?;
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct Args {
    node_id: String,
    socket: std::path::PathBuf,
    worker_control_socket: std::path::PathBuf,
    hardware_adapters: Vec<cy_adapter_client::HardwareAdapterEndpoint>,
    sandbox_adapter: cy_sandbox_client::SandboxAdapterEndpoint,
    installations_root: std::path::PathBuf,
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
        let mut hardware_adapters = Vec::new();
        let mut sandbox_adapter = None;
        let mut installations_root = PathBuf::from("/var/lib/cyrene/installations");
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
                "--hardware-adapter" => hardware_adapters.push(parse_hardware_adapter(&value()?)?),
                "--hardware-adapter-peer-uid" => {
                    let (adapter_id, uid) = parse_adapter_identity_value(&value()?)?;
                    adapter_peer_credentials.entry(adapter_id).or_default().uid = Some(uid);
                }
                "--hardware-adapter-peer-gid" => {
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
                "--runtime-journal" => runtime_journal = PathBuf::from(value()?),
                "--heartbeat-interval-ms" => heartbeat_interval = Duration::from_millis(value()?.parse()?),
                "--heartbeat-timeout-ms" => heartbeat_timeout = Duration::from_millis(value()?.parse()?),
                "--heartbeat-grace-ms" => heartbeat_grace = Duration::from_millis(value()?.parse()?),
                "--shutdown-ack-timeout-ms" => shutdown_ack_timeout = Duration::from_millis(value()?.parse()?),
                "--adapter-poll-interval-ms" => adapter_poll_interval = Duration::from_millis(value()?.parse()?),
                "--help" | "-h" => return Err(std::io::Error::other("usage: cyrene-kernel --sandbox-adapter ID=/absolute/socket.sock [--sandbox-adapter-peer-uid UID] [--sandbox-adapter-peer-gid GID] --hardware-adapter ID=/absolute/socket.sock [--hardware-adapter ID=/absolute/socket.sock] [--hardware-adapter-peer-uid ID=UID] [--hardware-adapter-peer-gid ID=GID] [--node-id ID] [--socket AUTHORITY_PATH] [--worker-control-socket PATH] [--installations-root PATH] [--runtime-journal PATH] [--heartbeat-interval-ms N] [--heartbeat-timeout-ms N] [--heartbeat-grace-ms N] [--shutdown-ack-timeout-ms N] [--adapter-poll-interval-ms N]").into()),
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
        if hardware_adapters.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "at least one --hardware-adapter ID=/absolute/socket.sock is required",
            )
            .into());
        }
        if !socket.is_absolute()
            || !worker_control_socket.is_absolute()
            || socket == worker_control_socket
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "authority and Worker-control socket paths must be distinct absolute paths",
            )
            .into());
        }
        for endpoint in &mut hardware_adapters {
            if let Some(credentials) = adapter_peer_credentials.remove(&endpoint.adapter_id) {
                endpoint.peer_credentials = credentials;
            }
        }
        if let Some(adapter_id) = adapter_peer_credentials.keys().next() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("peer credential policy references unknown hardware adapter: {adapter_id}"),
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
        require_configured_adapter_peers(&hardware_adapters, &sandbox_adapter)?;
        Ok(Self {
            node_id,
            socket,
            worker_control_socket,
            hardware_adapters,
            sandbox_adapter,
            installations_root,
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
    hardware_adapters: &[cy_adapter_client::HardwareAdapterEndpoint],
    sandbox_adapter: &cy_sandbox_client::SandboxAdapterEndpoint,
) -> std::io::Result<()> {
    for endpoint in hardware_adapters {
        if !endpoint.peer_credentials.is_configured() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "hardware adapter '{}' requires at least one --hardware-adapter-peer-uid or --hardware-adapter-peer-gid",
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
