//! Framework-owned worker protocol transport.
//!
//! sandboxd only exposes a byte stream backed by the worker's stdio. This
//! module owns framing, protocol identity checks, and response correlation at
//! the plugin boundary.

use std::{path::Path, time::Duration};

use bytes::BytesMut;
use cy_plugin_protocol::{Envelope, FramedCodec, ProtocolError, CURRENT_PROTOCOL_VERSION};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixStream,
    },
};

pub const MAX_WORKER_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum WorkerTransportError {
    #[error("worker transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker transport protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("worker transport connection timed out")]
    Timeout,
}

pub async fn connect(
    path: &Path,
    timeout: Duration,
) -> Result<(OwnedReadHalf, OwnedWriteHalf), WorkerTransportError> {
    let stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| WorkerTransportError::Timeout)??;
    Ok(stream.into_split())
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    envelope: &Envelope,
) -> Result<(), WorkerTransportError> {
    let codec = FramedCodec::new(MAX_WORKER_FRAME_BYTES);
    let frame = codec.encode(envelope)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    buffer: &mut BytesMut,
) -> Result<Option<Envelope>, WorkerTransportError> {
    let codec = FramedCodec::new(MAX_WORKER_FRAME_BYTES);
    let mut chunk = [0_u8; 8192];
    loop {
        if let Some((envelope, _)) = codec.decode(buffer)? {
            return Ok(Some(envelope));
        }
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

pub fn protocol_version_is_current(envelope: &Envelope) -> bool {
    envelope.protocol_version == CURRENT_PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn oversized_worker_frame_is_rejected_before_allocation() {
        let (mut writer, mut reader) = duplex(16);
        tokio::spawn(async move {
            writer
                .write_all(&((MAX_WORKER_FRAME_BYTES as u32) + 1).to_be_bytes())
                .await
                .expect("write oversized frame header");
        });
        let error = read_frame(&mut reader, &mut BytesMut::new())
            .await
            .expect_err("oversized frame must fail");
        assert!(matches!(
            error,
            WorkerTransportError::Protocol(ProtocolError::MessageTooLarge(..))
        ));
    }
}
