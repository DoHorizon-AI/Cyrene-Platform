// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-local-transport/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use async_trait::async_trait;
use bytes::BytesMut;
use cy_plugin_protocol::{Envelope, FramedCodec, ProtocolError};
use std::process::Stdio;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tracing::{error, info};

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("Transport closed")]
    Closed,
    #[error("Process exited with status: {0}")]
    ProcessExited(String),
}

/// Unified local transport interface for zero-port IPC messaging.
#[async_trait]
pub trait LocalTransport: Send + Sync {
    /// Send a framed protocol Envelope to the plugin.
    async fn send(&mut self, envelope: Envelope) -> Result<(), TransportError>;
    /// Receive the next framed protocol Envelope from the plugin.
    async fn receive(&mut self) -> Result<Option<Envelope>, TransportError>;
    /// Gracefully close the transport and terminate process if applicable.
    async fn close(&mut self) -> Result<(), TransportError>;
}

/// Stdio-based zero-port local transport implementation spawning an out-of-process plugin child.
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    read_rx: mpsc::Receiver<Result<Envelope, TransportError>>,
    codec: FramedCodec,
}

impl StdioTransport {
    /// Launch an out-of-process plugin via explicit binary executable and argument array.
    /// Do not concatenate a shell command string.
    pub fn spawn(executable: &str, args: &[&str]) -> Result<Self, TransportError> {
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            TransportError::Io(std::io::Error::other("Failed to open child stdin"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            TransportError::Io(std::io::Error::other("Failed to open child stdout"))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            TransportError::Io(std::io::Error::other("Failed to open child stderr"))
        })?;

        // Background task 1: Collect child stderr as logs line-by-line (stdout remains pure binary protocol)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                info!(target: "plugin_stderr", "{line}");
            }
        });

        // Background task 2: Read stdout binary frames and send to read_rx
        let (read_tx, read_rx) = mpsc::channel(100);
        let codec = FramedCodec::default();
        let codec_clone = FramedCodec::default();

        tokio::spawn(async move {
            let mut stdout = stdout;
            let mut buf = BytesMut::with_capacity(8192);
            let mut temp = [0u8; 4096];

            loop {
                match stdout.read(&mut temp).await {
                    Ok(0) => {
                        // EOF reached
                        break;
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&temp[..n]);
                        loop {
                            match codec_clone.decode(&mut buf) {
                                Ok(Some((envelope, _))) => {
                                    if read_tx.send(Ok(envelope)).await.is_err() {
                                        return;
                                    }
                                }
                                Ok(None) => break, // Need more data
                                Err(err) => {
                                    error!("Protocol framing error from plugin stdout: {err}");
                                    let _ = read_tx.send(Err(TransportError::Protocol(err))).await;
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = read_tx.send(Err(TransportError::Io(e))).await;
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            read_rx,
            codec,
        })
    }
}

#[async_trait]
impl LocalTransport for StdioTransport {
    async fn send(&mut self, envelope: Envelope) -> Result<(), TransportError> {
        let frame = self.codec.encode(&envelope)?;
        self.stdin.write_all(&frame).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<Envelope>, TransportError> {
        match self.read_rx.recv().await {
            Some(Ok(env)) => Ok(Some(env)),
            Some(Err(err)) => Err(err),
            None => Ok(None),
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.child.kill().await;
        Ok(())
    }
}

// Transport placeholder types for future optional socket-based local IPC
pub struct UnixSocketTransport;
pub struct WindowsNamedPipeTransport;
