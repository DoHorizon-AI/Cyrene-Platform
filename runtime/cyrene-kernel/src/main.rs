//! Linux composition root for the CYRENE Kernel daemon.
//!
//! The process owns no vendor driver code. It joins a versioned UDS hardware
//! adapter, creates only its delegated cgroup subtree, and serves Core v1 over
//! a local UDS endpoint with filesystem permissions as the trust boundary.

#[cfg(not(unix))]
fn main() {
    eprintln!("cyrene-kernel is supported only on Linux/Unix cgroup hosts");
    std::process::exit(1);
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener as StdUnixListener},
        sync::Arc,
    };

    use cy_installation_resolver::FilesystemInstalledPluginResolver;
    use cy_kernel_daemon::{KernelDaemon, KernelServiceAdapter, WorkerHeartbeatConfig};
    use cy_resource_manager::InMemoryResourceManager;
    use cy_sandbox::{CgroupV2Config, CgroupV2Runtime};
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::Server;

    let args = Args::parse()?;
    let cgroup_root = match args.cgroup_root {
        Some(root) => root,
        None => CgroupV2Config::delegated_root("cyrene")?,
    };
    let runtime = Arc::new(CgroupV2Runtime::new(CgroupV2Config {
        root: cgroup_root,
        device_bpf_enabled: !args.disable_device_bpf,
    }));
    runtime.initialize_owned_root()?;
    if !runtime.preflight_report().ready {
        return Err(std::io::Error::other(
            "cgroup v2 preflight did not meet required Kernel capabilities",
        )
        .into());
    }

    let resources = Arc::new(InMemoryResourceManager::new(&args.node_id, Vec::new()));
    let daemon = Arc::new(KernelDaemon::with_hardware_adapters(
        args.hardware_adapters,
        resources,
        runtime,
        &args.node_id,
        node_epoch(),
    )?);
    let heartbeat = WorkerHeartbeatConfig {
        socket_path: args.socket.clone(),
        interval: args.heartbeat_interval,
        timeout: args.heartbeat_timeout,
        graceful_stop: args.heartbeat_grace,
    };
    let adapter = KernelServiceAdapter::new(
        daemon,
        Arc::new(FilesystemInstalledPluginResolver::new(
            args.installations_root,
        )),
    )
    .with_worker_heartbeat(heartbeat);
    let _watchdog = adapter.start_watchdog();

    prepare_socket_path(&args.socket)?;
    let listener = StdUnixListener::bind(&args.socket)?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    fs::set_permissions(&args.socket, fs::Permissions::from_mode(0o660))?;
    Server::builder()
        .add_service(adapter.server())
        .add_service(adapter.lifecycle_server())
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct Args {
    node_id: String,
    socket: std::path::PathBuf,
    hardware_adapters: Vec<cy_adapter_client::HardwareAdapterEndpoint>,
    cgroup_root: Option<std::path::PathBuf>,
    installations_root: std::path::PathBuf,
    heartbeat_interval: std::time::Duration,
    heartbeat_timeout: std::time::Duration,
    heartbeat_grace: std::time::Duration,
    disable_device_bpf: bool,
}

#[cfg(unix)]
impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        use std::{env, path::PathBuf, time::Duration};
        let mut values = env::args().skip(1);
        let mut node_id = env::var("CYRENE_NODE_ID").unwrap_or_else(|_| "cyrene-node".to_string());
        let mut socket = PathBuf::from("/run/cyrene/kernel.sock");
        let mut hardware_adapters = Vec::new();
        let mut cgroup_root = None;
        let mut installations_root = PathBuf::from("/var/lib/cyrene/installations");
        let mut heartbeat_interval = Duration::from_secs(5);
        let mut heartbeat_timeout = Duration::from_secs(20);
        let mut heartbeat_grace = Duration::from_secs(10);
        let mut disable_device_bpf = false;
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
                "--hardware-adapter" => hardware_adapters.push(parse_hardware_adapter(&value()?)?),
                "--cgroup-root" => cgroup_root = Some(PathBuf::from(value()?)),
                "--installations-root" => installations_root = PathBuf::from(value()?),
                "--heartbeat-interval-ms" => heartbeat_interval = Duration::from_millis(value()?.parse()?),
                "--heartbeat-timeout-ms" => heartbeat_timeout = Duration::from_millis(value()?.parse()?),
                "--heartbeat-grace-ms" => heartbeat_grace = Duration::from_millis(value()?.parse()?),
                "--disable-device-bpf" => disable_device_bpf = true,
                "--help" | "-h" => return Err(std::io::Error::other("usage: cyrene-kernel --hardware-adapter ID=/absolute/socket.sock [--hardware-adapter ID=/absolute/socket.sock] [--node-id ID] [--socket PATH] [--cgroup-root PATH] [--installations-root PATH] [--heartbeat-interval-ms N] [--heartbeat-timeout-ms N] [--heartbeat-grace-ms N] [--disable-device-bpf]").into()),
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
        if hardware_adapters.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "at least one --hardware-adapter ID=/absolute/socket.sock is required",
            )
            .into());
        }
        Ok(Self {
            node_id,
            socket,
            hardware_adapters,
            cgroup_root,
            installations_root,
            heartbeat_interval,
            heartbeat_timeout,
            heartbeat_grace,
            disable_device_bpf,
        })
    }
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
fn prepare_socket_path(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
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
        )
        .into());
    }
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(unix)]
fn node_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
