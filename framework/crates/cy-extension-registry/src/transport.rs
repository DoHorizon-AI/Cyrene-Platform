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
    let deadline = tokio::time::Instant::now() + timeout;
    let poll_interval = Duration::from_millis(15);
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream.into_split()),
            Err(err) => {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(WorkerTransportError::Timeout);
                }
                match err.kind() {
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                        let remaining = deadline.saturating_duration_since(now);
                        if remaining.is_zero() {
                            return Err(WorkerTransportError::Timeout);
                        }
                        tokio::time::sleep(remaining.min(poll_interval)).await;
                    }
                    _ => return Err(WorkerTransportError::Io(err)),
                }
            }
        }
    }
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

use cy_plugin_protocol::pb::StreamItem;

pub const DEFAULT_STREAM_BUFFER_CHUNKS: usize = 32;

/// A bounded streaming chunk sender that applies backpressure on the producer.
pub struct StreamChunkSender {
    tx: tokio::sync::mpsc::Sender<StreamItem>,
}

impl StreamChunkSender {
    pub fn new(tx: tokio::sync::mpsc::Sender<StreamItem>) -> Self {
        Self { tx }
    }

    /// Sends a stream chunk with backpressure. If the downstream buffer is full,
    /// this future awaits until downstream consumer processes previous chunks.
    pub async fn send_chunk(&self, item: StreamItem) -> Result<(), WorkerTransportError> {
        self.tx.send(item).await.map_err(|_| {
            WorkerTransportError::Protocol(ProtocolError::FramingError(
                "stream receiver closed".to_string(),
            ))
        })
    }
}

/// A bounded stream receiver that yields chunks to consumers with backpressure control.
pub struct StreamChunkReceiver {
    rx: tokio::sync::mpsc::Receiver<StreamItem>,
}

impl StreamChunkReceiver {
    pub fn new(capacity: usize) -> (StreamChunkSender, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (StreamChunkSender::new(tx), Self { rx })
    }

    pub async fn recv_chunk(&mut self) -> Option<StreamItem> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::io::duplex;
    use tokio::net::UnixListener;

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

    #[tokio::test]
    async fn connect_retries_transient_not_found_until_bound() {
        let socket_path =
            std::env::temp_dir().join(format!("cyrene-retry-{}.sock", std::process::id()));
        let _ = fs::remove_file(&socket_path);

        let path_clone = socket_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _listener = UnixListener::bind(&path_clone).expect("bind delayed test socket");
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let res = connect(&socket_path, Duration::from_millis(500)).await;
        assert!(
            res.is_ok(),
            "connect should succeed after retry: {:?}",
            res.err()
        );
        let _ = fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn test_streaming_bounded_backpressure_flow_control() {
        // Buffer capacity of 2 chunks
        let (sender, mut receiver) = StreamChunkReceiver::new(2);
        let sent_count = Arc::new(AtomicU32::new(0));
        let sent_clone = sent_count.clone();

        let producer = tokio::spawn(async move {
            for i in 0..5 {
                let item = StreamItem {
                    request_id: "req-bp".to_string(),
                    sequence_number: i + 1,
                    is_last: i == 4,
                    data: None,
                };
                sender.send_chunk(item).await.unwrap();
                sent_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        // Give producer time to send. Since capacity is 2, producer can send at most 2 (or 3 if 1 in flight).
        tokio::time::sleep(Duration::from_millis(30)).await;
        let count_before = sent_count.load(Ordering::SeqCst);
        assert!(
            count_before <= 3,
            "producer must be blocked by backpressure when buffer is full (sent: {})",
            count_before
        );

        // Consumer reads chunks one by one
        let mut received = 0;
        while let Some(item) = receiver.recv_chunk().await {
            received += 1;
            if item.is_last {
                break;
            }
        }

        producer.await.unwrap();
        assert_eq!(received, 5);
        assert_eq!(sent_count.load(Ordering::SeqCst), 5);
    }
}
