//! Process entry point for the NVIDIA hardware adapter sidecar.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use cy_proto::hardware_v1;
    use cyrene_nvidia_adapter::{discovery::NvidiaSmiProvider, handle_request};
    use prost::Message;
    use std::{
        env, fs,
        io::{Read, Write},
        os::unix::{
            fs::FileTypeExt,
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
    };

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let socket_path = socket_argument(env::args().skip(1))
        .unwrap_or_else(|| PathBuf::from("/run/cyrene/nvidia-adapter.sock"));
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
    let provider = NvidiaSmiProvider::new("nvidia-smi");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = serve_one(stream, &provider);
            }
            Err(error) => eprintln!("adapter accept failed: {error}"),
        }
    }

    fn socket_argument(arguments: impl Iterator<Item = String>) -> Option<PathBuf> {
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            if argument == "--socket" {
                return arguments.next().map(PathBuf::from);
            }
        }
        None
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
