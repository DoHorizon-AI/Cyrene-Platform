// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/nvidia/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Process entry point for the NVIDIA hardware adapter sidecar.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use cy_proto::hardware_v1;
    use cyrene_nvidia_adapter::{discovery::NvidiaSmiProvider, handle_request, verify_client_peer};
    use prost::Message;
    use std::{
        env, fs,
        io::{Read, Write},
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
    };

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let (socket_path, allowed_client_uid, allowed_client_gid) =
        adapter_arguments(env::args().skip(1))?;
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(&socket_path)?;
        if !metadata.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket adapter path",
            ));
        }
        fs::remove_file(&socket_path)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o660))?;
    // `nvidia` is the stable UDS adapter identity. `nvidia-smi` remains an
    // implementation detail of this external process and never leaks into
    // Kernel routing configuration.
    let provider = NvidiaSmiProvider::new("nvidia");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) =
                    verify_client_peer(&stream, allowed_client_uid, allowed_client_gid)
                {
                    eprintln!("adapter rejected UDS peer: {error}");
                    continue;
                }
                let _ = serve_one(stream, &provider);
            }
            Err(error) => eprintln!("adapter accept failed: {error}"),
        }
    }

    fn adapter_arguments(
        arguments: impl Iterator<Item = String>,
    ) -> std::io::Result<(PathBuf, Option<u32>, Option<u32>)> {
        let mut arguments = arguments;
        let mut socket_path = PathBuf::from("/run/cyrene/nvidia-adapter.sock");
        let mut allowed_client_uid = None;
        let mut allowed_client_gid = None;
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments.next().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{argument} requires a value"),
                    )
                })
            };
            match argument.as_str() {
                "--socket" => socket_path = PathBuf::from(value()?),
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
                "--help" | "-h" => {
                    return Err(std::io::Error::other(
                        "usage: cyrene-nvidia-adapter [--socket PATH] [--allowed-client-uid UID] [--allowed-client-gid GID]",
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
        // Fail-closed: the adapter must be configured with at least one trusted
        // Kernel peer UID/GID; otherwise UDS admission silently allows any local
        // user able to reach the socket.
        if allowed_client_uid.is_none() && allowed_client_gid.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cyrene-nvidia-adapter requires at least one of --allowed-client-uid or --allowed-client-gid to enforce UDS admission",
            ));
        }
        Ok((socket_path, allowed_client_uid, allowed_client_gid))
    }

    fn serve_one(mut stream: UnixStream, provider: &NvidiaSmiProvider) -> std::io::Result<()> {
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
                "adapter frame exceeds limit",
            ));
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload)?;
        Ok(payload)
    }

    fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
        if payload.len() > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "adapter frame exceeds limit",
            ));
        }
        writer.write_all(&(payload.len() as u32).to_be_bytes())?;
        writer.write_all(payload)
    }

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("cyrene-nvidia-adapter requires a Unix host with Unix domain sockets");
}
