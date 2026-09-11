// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-adapter-client/src/transport.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use std::{
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use cy_kernel_contract::ProviderError;

#[cfg(unix)]
use crate::credential::verify_connected_peer;
use crate::credential::PeerCredentialExpectation;

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)
}

pub fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
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

pub(crate) fn exchange(
    adapter_id: &str,
    socket_path: &Path,
    timeout: Duration,
    peer_credentials: PeerCredentialExpectation,
    payload: &[u8],
) -> Result<Vec<u8>, ProviderError> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(socket_path).map_err(|error| {
            ProviderError::new(
                adapter_id,
                "ADAPTER_UNAVAILABLE",
                &format!("{}: {error}", socket_path.display()),
            )
        })?;
        verify_connected_peer(adapter_id, &stream, peer_credentials)?;
        stream.set_read_timeout(Some(timeout)).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_CONFIG", &error.to_string())
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_CONFIG", &error.to_string())
        })?;
        write_frame(&mut stream, payload).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_WRITE", &error.to_string())
        })?;
        read_frame(&mut stream).map_err(|error| {
            ProviderError::new(adapter_id, "ADAPTER_TRANSPORT_READ", &error.to_string())
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (socket_path, timeout, peer_credentials, payload);
        Err(ProviderError::new(
            adapter_id,
            "ADAPTER_UDS_UNSUPPORTED",
            "Unix domain sockets require a Unix Kernel host",
        ))
    }
}
