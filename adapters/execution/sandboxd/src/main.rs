// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Privileged local Sandbox Adapter Host process.

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Args {
    adapter_id: String,
    socket: std::path::PathBuf,
    cgroup_root: Option<std::path::PathBuf>,
    transport_root: std::path::PathBuf,
    disable_device_bpf: bool,
    dev_mode: bool,
    allowed_client_uid: Option<u32>,
    allowed_client_gid: Option<u32>,
}

#[cfg(target_os = "linux")]
impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::parse_from_with_identity_resolvers(
            std::env::args().skip(1),
            cyrene_runtime_identity::resolve_uid_selector,
            cyrene_runtime_identity::resolve_gid_selector,
        )?)
    }

    fn parse_from_with_identity_resolvers(
        values: impl IntoIterator<Item = String>,
        mut resolve_uid: impl FnMut(&str) -> std::io::Result<u32>,
        mut resolve_gid: impl FnMut(&str) -> std::io::Result<u32>,
    ) -> std::io::Result<Self> {
        use std::path::PathBuf;

        let mut values = values.into_iter();
        let mut adapter_id = "sandboxd".to_string();
        let mut socket = PathBuf::from("/run/cyrene/sandboxd.sock");
        let mut cgroup_root = None;
        let mut transport_root = PathBuf::from("/run/cyrene/workers");
        let mut disable_device_bpf = false;
        let mut dev_mode = false;
        let mut allowed_client_uid = None;
        let mut allowed_client_gid = None;
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
                "--adapter-id" => adapter_id = value()?,
                "--socket" => socket = PathBuf::from(value()?),
                "--cgroup-root" => cgroup_root = Some(PathBuf::from(value()?)),
                "--transport-root" => transport_root = PathBuf::from(value()?),
                "--disable-device-bpf" => disable_device_bpf = true,
                "--dev-mode" => dev_mode = true,
                "--allowed-client-uid" => allowed_client_uid = Some(resolve_uid(&value()?)?),
                "--allowed-client-gid" => allowed_client_gid = Some(resolve_gid(&value()?)?),
                "--help" | "-h" => {
                    return Err(std::io::Error::other(
                        "usage: cyrene-sandboxd [--adapter-id ID] [--socket PATH] [--cgroup-root PATH] [--disable-device-bpf] [--dev-mode] [--allowed-client-uid UID_OR_ACCOUNT] [--allowed-client-gid GID_OR_GROUP]",
                    ));
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown argument: {argument}"),
                    ));
                }
            }
        }
        if adapter_id.is_empty()
            || !adapter_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || !socket.is_absolute()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "sandbox adapter ID must be safe and socket path must be absolute",
            ));
        }
        // In dev mode, default allowed_client_uid to own UID if not specified.
        // 中文：开发模式下，如果未指定 allowed_client_uid，则默认使用当前进程 UID。
        if dev_mode && allowed_client_uid.is_none() && allowed_client_gid.is_none() {
            allowed_client_uid = Some(unsafe { libc::getuid() });
        }
        // Fail closed when no trusted Kernel peer identity is configured.
        // 中文：未配置受信任的 Kernel 对端身份时必须失败关闭。
        if allowed_client_uid.is_none() && allowed_client_gid.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cyrene-sandboxd requires at least one of --allowed-client-uid or --allowed-client-gid to enforce UDS admission",
            ));
        }
        Ok(Self {
            adapter_id,
            socket,
            cgroup_root,
            transport_root,
            disable_device_bpf,
            dev_mode,
            allowed_client_uid,
            allowed_client_gid,
        })
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use cy_proto::sandbox_v1;
    use cyrene_sandboxd::{handle_request, verify_client_peer, CgroupV2Config, CgroupV2Runtime};
    use prost::Message;
    use std::{
        fs,
        io::{Read, Write},
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        sync::Arc,
    };

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let init_observability = || {
        let log_format = std::env::var("CYRENE_LOG_FORMAT")
            .ok()
            .and_then(|format| format.parse().ok())
            .unwrap_or(cy_observability::LogFormat::Json);
        let log_level = std::env::var("CYRENE_LOG_LEVEL")
            .or_else(|_| std::env::var("RUST_LOG"))
            .unwrap_or_else(|_| "info".to_string());
        let obs_config = cy_observability::ObservabilityConfig::managed("cyrene-sandboxd")
            .with_format(log_format)
            .with_log_level(log_level);
        cy_observability::init_observability(obs_config).ok()
    };

    let args = match Args::parse() {
        Ok(a) => a,
        Err(err) => {
            let _guard = init_observability();
            tracing::error!(
                event.name = "platform.service.startup_failed",
                error.code = cy_observability::PlatformErrorCode::SandboxCgroupInitFailed.as_str(),
                message = "Sandbox host argument parsing failed",
                error = %err,
            );
            return Err(err);
        }
    };
    let uses_delegated_default = args.cgroup_root.is_none() && !args.dev_mode;
    let root = match args.cgroup_root {
        Some(root) => root,
        None => {
            if args.dev_mode {
                let uid = unsafe { libc::getuid() };
                std::env::temp_dir().join(format!("cyrene-dev-cgroup-{}", uid))
            } else {
                CgroupV2Config::delegated_root("cyrene")?
            }
        }
    };
    let runtime = Arc::new(CgroupV2Runtime::new(CgroupV2Config {
        root,
        transport_root: args.transport_root.clone(),
        device_bpf_enabled: !args.disable_device_bpf && !args.dev_mode,
        dev_mode: args.dev_mode,
    }));
    if uses_delegated_default {
        runtime.initialize_delegated_owned_root()?;
    } else {
        runtime.initialize_owned_root()?;
    }
    let _guard = init_observability();

    if let Some(parent) = args.socket.parent() {
        fs::create_dir_all(parent)?;
    }
    if args.socket.exists() {
        let metadata = fs::symlink_metadata(&args.socket)?;
        if !metadata.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket sandbox adapter path",
            )
            .into());
        }
        fs::remove_file(&args.socket)?;
    }
    let listener = UnixListener::bind(&args.socket)?;
    fs::set_permissions(&args.socket, fs::Permissions::from_mode(0o660))?;
    tracing::info!(
        event.name = cy_observability::EVENT_SERVICE_STARTED,
        message = "Starting Cyrene Sandbox Adapter Host",
        adapter_id = %args.adapter_id,
        socket = %args.socket.display(),
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) =
                    verify_client_peer(&stream, args.allowed_client_uid, args.allowed_client_gid)
                {
                    tracing::warn!(
                        event.name = "platform.sandbox.peer_rejected",
                        adapter_id = %args.adapter_id,
                        error = %error,
                        message = "Sandbox rejected UDS peer credential",
                    );
                    continue;
                }
                if let Err(error) = serve_one(stream, runtime.as_ref(), &args.adapter_id) {
                    tracing::error!(
                        event.name = "platform.sandbox.request_failed",
                        error.code = cy_observability::PlatformErrorCode::SandboxKillFailed.as_str(),
                        adapter_id = %args.adapter_id,
                        error = %error,
                        message = "Sandbox request processing failed",
                    );
                }
            }
            Err(error) => {
                tracing::error!(
                    event.name = "platform.sandbox.accept_failed",
                    error.code = cy_observability::PlatformErrorCode::SandboxCgroupInitFailed.as_str(),
                    adapter_id = %args.adapter_id,
                    error = %error,
                    message = "Sandbox listener accept failed",
                );
            }
        }
    }

    fn serve_one(
        mut stream: UnixStream,
        runtime: &CgroupV2Runtime,
        adapter_id: &str,
    ) -> std::io::Result<()> {
        let payload = read_frame(&mut stream)?;
        let request = sandbox_v1::SandboxRequest::decode(payload.as_slice())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let response = handle_request(runtime, adapter_id, request).encode_to_vec();
        write_frame(&mut stream, &response)
    }

    fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
        let mut length = [0_u8; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "sandbox adapter frame exceeds limit",
            ));
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload)?;
        Ok(payload)
    }

    fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
        if payload.len() > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "sandbox adapter frame exceeds limit",
            ));
        }
        writer.write_all(&(payload.len() as u32).to_be_bytes())?;
        writer.write_all(payload)
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cyrene-sandboxd requires a Linux host with cgroup v2 and Unix domain sockets");
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Args;

    fn shipped_sandboxd_arguments() -> Vec<String> {
        let command = include_str!("../../../../infrastructure/systemd/cyrene-sandboxd.service")
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .expect("the shipped sandboxd unit has an ExecStart");
        let command_arguments = command.split_whitespace().collect::<Vec<_>>();
        let separator = command_arguments
            .iter()
            .position(|argument| *argument == "--")
            .expect("the component runner separates daemon arguments");
        command_arguments[separator + 1..]
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    #[test]
    fn shipped_sandboxd_unit_resolves_named_kernel_peer_with_exact_ids() {
        let args = Args::parse_from_with_identity_resolvers(
            shipped_sandboxd_arguments(),
            |selector| match selector {
                "cyrene-kernel" => Ok(1843),
                other => other.parse::<u32>().map_err(std::io::Error::other),
            },
            |selector| match selector {
                "cyrene" => Ok(2764),
                other => other.parse::<u32>().map_err(std::io::Error::other),
            },
        )
        .expect("the shipped sandboxd unit must resolve its symbolic peer selectors");

        assert_eq!(args.allowed_client_uid, Some(1843));
        assert_eq!(args.allowed_client_gid, Some(2764));
    }
}
