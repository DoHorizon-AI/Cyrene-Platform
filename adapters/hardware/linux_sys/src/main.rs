// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/linux_sys/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CYRENE Linux System Adapter Host daemon binary.

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        env, fs,
        io::{Read, Write},
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
    };

    use cy_proto::hardware_v1;
    use cyrene_linux_sys_adapter::{handle_request, verify_client_peer, LinuxSystemProvider};
    use prost::Message;

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let args = Args::parse()?;
    let provider = LinuxSystemProvider::new(&args.adapter_id);

    if let Some(parent) = args.socket.parent() {
        fs::create_dir_all(parent)?;
    }
    if args.socket.exists() {
        let metadata = fs::symlink_metadata(&args.socket)?;
        if !metadata.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket adapter path",
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
                        "linux-sys adapter rejected UDS peer: adapter_id={} error={error}",
                        args.adapter_id
                    );
                    continue;
                }
                if let Err(error) = serve_one(stream, &provider) {
                    eprintln!("linux-sys adapter request failed: {error}");
                }
            }
            Err(error) => eprintln!("linux-sys adapter accept failed: {error}"),
        }
    }

    fn serve_one(mut stream: UnixStream, provider: &LinuxSystemProvider) -> std::io::Result<()> {
        let payload = read_frame(&mut stream)?;
        let request = hardware_v1::AdapterRequest::decode(payload.as_slice())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let response = handle_request(provider, request).encode_to_vec();
        write_frame(&mut stream, &response)
    }

    fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
        let mut length = [0_u8; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "linux-sys adapter frame exceeds limit",
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
                "linux-sys adapter frame exceeds limit",
            ));
        }
        let length = (payload.len() as u32).to_be_bytes();
        writer.write_all(&length)?;
        writer.write_all(payload)?;
        writer.flush()
    }

    struct Args {
        socket: PathBuf,
        adapter_id: String,
        allowed_client_uid: Option<u32>,
        allowed_client_gid: Option<u32>,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn std::error::Error>> {
            let mut socket = PathBuf::from("/run/cyrene/adapters/linux-sys.sock");
            let mut adapter_id = "hardware-adapter-linux-sys".to_string();
            let mut allowed_client_uid = None;
            let mut allowed_client_gid = None;

            let mut args = env::args().skip(1);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--socket" => {
                        socket = PathBuf::from(args.next().ok_or("missing --socket argument")?);
                    }
                    "--adapter-id" => {
                        adapter_id = args.next().ok_or("missing --adapter-id argument")?;
                    }
                    "--allowed-client-uid" => {
                        allowed_client_uid = Some(
                            args.next()
                                .ok_or("missing --allowed-client-uid argument")?
                                .parse()?,
                        );
                    }
                    "--allowed-client-gid" => {
                        allowed_client_gid = Some(
                            args.next()
                                .ok_or("missing --allowed-client-gid argument")?
                                .parse()?,
                        );
                    }
                    "--help" | "-h" => {
                        println!("Usage: cyrene-linux-sys-adapter [--socket PATH] [--adapter-id ID] [--allowed-client-uid UID] [--allowed-client-gid GID]");
                        std::process::exit(0);
                    }
                    other => {
                        return Err(format!("unknown argument: {other}").into());
                    }
                }
            }

            Ok(Self {
                socket,
                adapter_id,
                allowed_client_uid,
                allowed_client_gid,
            })
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cyrene-linux-sys-adapter requires a Linux platform.");
    std::process::exit(1);
}
