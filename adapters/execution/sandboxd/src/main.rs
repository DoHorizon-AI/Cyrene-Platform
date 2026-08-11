//! Privileged local Sandbox Adapter Host process.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use cy_proto::sandbox_v1;
    use cyrene_sandboxd::{handle_request, CgroupV2Config, CgroupV2Runtime};
    use prost::Message;
    use std::{
        env, fs,
        io::{Read, Write},
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
        sync::Arc,
    };

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let args = Args::parse()?;
    let root = match args.cgroup_root {
        Some(root) => root,
        None => CgroupV2Config::delegated_root("cyrene")?,
    };
    let runtime = Arc::new(CgroupV2Runtime::new(CgroupV2Config {
        root,
        device_bpf_enabled: !args.disable_device_bpf,
    }));
    runtime.initialize_owned_root()?;

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
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) =
                    verify_client_peer(&stream, args.allowed_client_uid, args.allowed_client_gid)
                {
                    eprintln!(
                        "sandboxd rejected UDS peer: adapter_id={} error={error}",
                        args.adapter_id
                    );
                    continue;
                }
                if let Err(error) = serve_one(stream, runtime.as_ref(), &args.adapter_id) {
                    eprintln!("sandboxd request failed: {error}");
                }
            }
            Err(error) => eprintln!("sandboxd accept failed: {error}"),
        }
    }

    fn verify_client_peer(
        stream: &UnixStream,
        expected_uid: Option<u32>,
        expected_gid: Option<u32>,
    ) -> std::io::Result<()> {
        if expected_uid.is_none() && expected_gid.is_none() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            let credentials =
                nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                    .map_err(std::io::Error::other)?;
            if expected_uid.is_some_and(|uid| uid != credentials.uid())
                || expected_gid.is_some_and(|gid| gid != credentials.gid())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "UDS peer credentials do not match configured Kernel identity",
                ));
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = stream;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "UDS peer credential checks require Linux",
            ))
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

    #[derive(Debug)]
    struct Args {
        adapter_id: String,
        socket: PathBuf,
        cgroup_root: Option<PathBuf>,
        disable_device_bpf: bool,
        allowed_client_uid: Option<u32>,
        allowed_client_gid: Option<u32>,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn std::error::Error>> {
            let mut values = env::args().skip(1);
            let mut adapter_id = "sandboxd".to_string();
            let mut socket = PathBuf::from("/run/cyrene/sandboxd.sock");
            let mut cgroup_root = None;
            let mut disable_device_bpf = false;
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
                    "--disable-device-bpf" => disable_device_bpf = true,
                    "--allowed-client-uid" => {
                        allowed_client_uid = Some(value()?.parse::<u32>().map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "--allowed-client-uid must be an unsigned integer",
                            )
                        })?)
                    }
                    "--allowed-client-gid" => {
                        allowed_client_gid = Some(value()?.parse::<u32>().map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "--allowed-client-gid must be an unsigned integer",
                            )
                        })?)
                    }
                    "--help" | "-h" => return Err(std::io::Error::other("usage: cyrene-sandboxd [--adapter-id ID] [--socket PATH] [--cgroup-root PATH] [--disable-device-bpf] [--allowed-client-uid UID] [--allowed-client-gid GID]").into()),
                    _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("unknown argument: {argument}")).into()),
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
                )
                .into());
            }
            Ok(Self {
                adapter_id,
                socket,
                cgroup_root,
                disable_device_bpf,
                allowed_client_uid,
                allowed_client_gid,
            })
        }
    }

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("cyrene-sandboxd requires a Unix host with cgroup v2 and Unix domain sockets");
}
