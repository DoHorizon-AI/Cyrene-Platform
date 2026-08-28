//! Generic capability worker activation, supervision, lifecycle framing, and RPC client.
//!
//! This module provides a Product-neutral, capability-agnostic worker activation layer
//! for capabilities with `ExecutionMode::Worker`. It reuses the single canonical
//! `cy.plugin.v1` Protobuf envelope framing over standard I/O streams.

use std::{
    collections::HashMap,
    io::{BufReader, BufWriter, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use cy_manifest::{PluginManifest, Runtime};
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{
        Cancel, Envelope, Hello, Invoke,
        PluginErrorPayload, Shutdown,
    },
    plugin_error_payload, CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_MESSAGE_BYTES,
};
use prost::Message;
use thiserror::Error;
use uuid::Uuid;

use crate::media::{
    ImageInspection, InspectImageRequest, MediaProcessor, MediaProcessorError,
    TransformImageRequest, TransformedImage,
};

/// Cooperative cancellation token passed to worker operations.
pub trait CancellationToken: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// A cancellation token for callers that never cancel.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverCancelled;

impl CancellationToken for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Thread-safe atomic cancellation token that can be triggered asynchronously.
#[derive(Debug, Clone, Default)]
pub struct AtomicCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl AtomicCancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl CancellationToken for AtomicCancellationToken {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Strongly-typed generic terminal error categories for Product adapters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkerTerminalError {
    #[error("worker unavailable: {0}")]
    WorkerUnavailable(String),
    #[error("activation failed: {0}")]
    ActivationFailed(String),
    #[error("protocol mismatch: {0}")]
    ProtocolMismatch(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("capability execution failure: {0}")]
    CapabilityExecutionFailure(String),
    #[error("worker crashed: {0}")]
    WorkerCrashed(String),
    #[error("request timed out: {0}")]
    Timeout(String),
    #[error("operation cancelled: {0}")]
    Cancelled(String),
}

impl WorkerTerminalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::WorkerUnavailable(_) => "WORKER_UNAVAILABLE",
            Self::ActivationFailed(_) => "ACTIVATION_FAILED",
            Self::ProtocolMismatch(_) => "PROTOCOL_MISMATCH",
            Self::InvalidRequest(_) => "INVALID_REQUEST",
            Self::CapabilityExecutionFailure(_) => "CAPABILITY_EXECUTION_FAILURE",
            Self::WorkerCrashed(_) => "WORKER_CRASHED",
            Self::Timeout(_) => "TIMEOUT",
            Self::Cancelled(_) => "CANCELLED",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::WorkerUnavailable(m)
            | Self::ActivationFailed(m)
            | Self::ProtocolMismatch(m)
            | Self::InvalidRequest(m)
            | Self::CapabilityExecutionFailure(m)
            | Self::WorkerCrashed(m)
            | Self::Timeout(m)
            | Self::Cancelled(m) => m,
        }
    }
}

/// Options configuring worker process activation.
#[derive(Debug, Clone)]
pub struct WorkerActivationOptions {
    pub working_dir: Option<PathBuf>,
    pub python_path: Vec<PathBuf>,
    pub python_executable: Option<String>,
    pub environment: HashMap<String, String>,
    pub handshake_timeout: Duration,
    pub default_invoke_timeout: Duration,
    pub shutdown_grace_period: Duration,
    pub max_message_bytes: usize,
}

impl Default for WorkerActivationOptions {
    fn default() -> Self {
        Self {
            working_dir: None,
            python_path: Vec::new(),
            python_executable: None,
            environment: HashMap::new(),
            handshake_timeout: Duration::from_secs(5),
            default_invoke_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_secs(2),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

/// Read one 4-byte BE length-prefixed Protobuf Envelope from reader.
pub fn read_frame<R: Read>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<Envelope>, WorkerTerminalError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => {
            return Err(WorkerTerminalError::WorkerCrashed(format!(
                "failed to read frame length: {e}"
            )))
        }
    }

    let payload_len = u32::from_be_bytes(len_buf) as usize;
    if payload_len > max_bytes {
        return Err(WorkerTerminalError::ProtocolMismatch(format!(
            "frame size {payload_len} exceeds max {max_bytes}"
        )));
    }

    let mut payload = vec![0u8; payload_len];
    if let Err(e) = reader.read_exact(&mut payload) {
        return Err(WorkerTerminalError::WorkerCrashed(format!(
            "failed to read frame payload: {e}"
        )));
    }

    let envelope = Envelope::decode(&payload[..]).map_err(|e| {
        WorkerTerminalError::ProtocolMismatch(format!("protobuf decode failed: {e}"))
    })?;
    Ok(Some(envelope))
}

/// Write one 4-byte BE length-prefixed Protobuf Envelope to writer and flush.
pub fn write_frame<W: Write>(
    writer: &mut W,
    envelope: &Envelope,
    max_bytes: usize,
) -> Result<(), WorkerTerminalError> {
    let payload_len = envelope.encoded_len();
    if payload_len > max_bytes {
        return Err(WorkerTerminalError::ProtocolMismatch(format!(
            "envelope size {payload_len} exceeds max {max_bytes}"
        )));
    }

    let len_bytes = (payload_len as u32).to_be_bytes();
    writer
        .write_all(&len_bytes)
        .map_err(|e| WorkerTerminalError::WorkerCrashed(format!("failed to write frame length: {e}")))?;
    let mut buf = Vec::with_capacity(payload_len);
    envelope.encode(&mut buf).map_err(|e| {
        WorkerTerminalError::ProtocolMismatch(format!("protobuf encode failed: {e}"))
    })?;
    writer
        .write_all(&buf)
        .map_err(|e| WorkerTerminalError::WorkerCrashed(format!("failed to write payload: {e}")))?;
    writer
        .flush()
        .map_err(|e| WorkerTerminalError::WorkerCrashed(format!("failed to flush writer: {e}")))?;
    Ok(())
}

/// Active supervised client for an out-of-process capability worker.
pub struct CapabilityWorkerClient {
    child: Option<Child>,
    writer: BufWriter<ChildStdin>,
    reader_rx: Receiver<Result<Envelope, WorkerTerminalError>>,
    plugin_id: String,
    plugin_version: String,
    api_version: String,
    declared_capabilities: Vec<String>,
    sequence_number: u64,
    options: WorkerActivationOptions,
    is_shut_down: bool,
}

impl CapabilityWorkerClient {
    pub fn new(
        mut child: Child,
        plugin_id: String,
        plugin_version: String,
        api_version: String,
        declared_capabilities: Vec<String>,
        options: WorkerActivationOptions,
    ) -> Result<Self, WorkerTerminalError> {
        let stdin = child.stdin.take().ok_or_else(|| {
            WorkerTerminalError::ActivationFailed("child process stdin not piped".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            WorkerTerminalError::ActivationFailed("child process stdout not piped".into())
        })?;

        let max_bytes = options.max_message_bytes;
        let (reader_tx, reader_rx) = mpsc::channel();
        thread::Builder::new()
            .name(format!("worker-reader-{}", plugin_id))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_frame(&mut reader, max_bytes) {
                        Ok(Some(env)) => {
                            if reader_tx.send(Ok(env)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = reader_tx.send(Err(WorkerTerminalError::WorkerCrashed(
                                "worker process closed stream (EOF)".into(),
                            )));
                            break;
                        }
                        Err(err) => {
                            let _ = reader_tx.send(Err(err));
                            break;
                        }
                    }
                }
            })
            .map_err(|e| {
                WorkerTerminalError::ActivationFailed(format!("failed to spawn reader thread: {e}"))
            })?;

        Ok(Self {
            child: Some(child),
            writer: BufWriter::new(stdin),
            reader_rx,
            plugin_id,
            plugin_version,
            api_version,
            declared_capabilities,
            sequence_number: 0,
            options,
            is_shut_down: false,
        })
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    pub fn declared_capabilities(&self) -> &[String] {
        &self.declared_capabilities
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence_number = self.sequence_number.wrapping_add(1);
        self.sequence_number
    }

    /// Read next envelope with timeout and cancellation check.
    fn read_envelope_timed(
        &mut self,
        timeout: Duration,
        cancellation: &dyn CancellationToken,
    ) -> Result<Envelope, WorkerTerminalError> {
        let start = Instant::now();
        let slice = Duration::from_millis(20);

        while start.elapsed() < timeout {
            if cancellation.is_cancelled() {
                return Err(WorkerTerminalError::Cancelled("operation cancelled".into()));
            }

            match self.reader_rx.recv_timeout(slice) {
                Ok(Ok(env)) => return Ok(env),
                Ok(Err(err)) => return Err(err),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(WorkerTerminalError::WorkerCrashed(
                        "worker communication channel disconnected".into(),
                    ))
                }
            }
        }

        Err(WorkerTerminalError::Timeout(format!(
            "request timed out after {}ms",
            timeout.as_millis()
        )))
    }

    /// Invoke a typed capability RPC method on the worker.
    pub fn invoke(
        &mut self,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
        cancellation: &dyn CancellationToken,
    ) -> Result<Vec<u8>, WorkerTerminalError> {
        if self.is_shut_down {
            return Err(WorkerTerminalError::WorkerUnavailable(
                "worker client is already shut down".into(),
            ));
        }

        // 1. Pre-cancellation check
        if cancellation.is_cancelled() {
            return Err(WorkerTerminalError::Cancelled(
                "pre-invocation cancellation".into(),
            ));
        }

        let request_id = Uuid::new_v4().to_string();
        let seq = self.next_sequence();
        let invoke_env = Envelope {
            request_id: request_id.clone(),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: seq,
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::Invoke(Invoke {
                extension_point: capability.to_string(),
                method: method.to_string(),
                payload: payload.to_vec(),
                request: None,
            })),
        };

        write_frame(&mut self.writer, &invoke_env, self.options.max_message_bytes)?;

        // 2. Await response or cooperative in-flight cancellation
        let start = Instant::now();
        let slice = Duration::from_millis(20);

        while start.elapsed() < timeout {
            if cancellation.is_cancelled() {
                // Send explicit cooperative cancellation
                let cancel_seq = self.next_sequence();
                let cancel_env = Envelope {
                    request_id: format!("cancel-{request_id}"),
                    trace_id: String::new(),
                    plugin_id: self.plugin_id.clone(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: cancel_seq,
                    generation: 1,
                    fence_token: 1,
                    payload: Some(Payload::Cancel(Cancel {
                        target_request_id: request_id.clone(),
                        reason: "caller requested cancellation".to_string(),
                    })),
                };
                let _ = write_frame(
                    &mut self.writer,
                    &cancel_env,
                    self.options.max_message_bytes,
                );

                // Drain until we get CancelAck or terminal response or timeout
                let cancel_deadline = Instant::now() + Duration::from_millis(500);
                while Instant::now() < cancel_deadline {
                    if let Ok(Ok(resp)) = self.reader_rx.recv_timeout(Duration::from_millis(50)) {
                        if resp.request_id == request_id
                            || resp.request_id == format!("cancel-{request_id}")
                        {
                            break;
                        }
                    }
                }

                return Err(WorkerTerminalError::Cancelled(
                    "operation cancelled in flight".into(),
                ));
            }

            match self.reader_rx.recv_timeout(slice) {
                Ok(Ok(response)) => {
                    if response.request_id != request_id {
                        continue;
                    }
                    match response.payload {
                        Some(Payload::InvokeResult(result)) => return Ok(result.payload),
                        Some(Payload::Error(err)) => {
                            return Err(Self::map_error_payload(err));
                        }
                        other => {
                            return Err(WorkerTerminalError::ProtocolMismatch(format!(
                                "unexpected response payload: {other:?}"
                            )));
                        }
                    }
                }
                Ok(Err(err)) => return Err(err),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(WorkerTerminalError::WorkerCrashed(
                        "worker communication stream broken".into(),
                    ))
                }
            }
        }

        Err(WorkerTerminalError::Timeout(format!(
            "invocation timed out after {}ms",
            timeout.as_millis()
        )))
    }

    fn map_error_payload(err: PluginErrorPayload) -> WorkerTerminalError {
        match err.code {
            code if code == plugin_error_payload::Code::InvalidInput as i32 => {
                WorkerTerminalError::InvalidRequest(err.message)
            }
            code if code == plugin_error_payload::Code::Cancelled as i32 => {
                WorkerTerminalError::Cancelled(err.message)
            }
            code if code == plugin_error_payload::Code::ProtocolError as i32 => {
                WorkerTerminalError::ProtocolMismatch(err.message)
            }
            code if code == plugin_error_payload::Code::Timeout as i32 => {
                WorkerTerminalError::Timeout(err.message)
            }
            _ => {
                if err.message.contains("INVALID_INPUT") || err.message.contains("UNSUPPORTED_INPUT") {
                    WorkerTerminalError::InvalidRequest(err.message)
                } else if err.message.contains("CANCELLED") {
                    WorkerTerminalError::Cancelled(err.message)
                } else {
                    WorkerTerminalError::CapabilityExecutionFailure(err.message)
                }
            }
        }
    }

    /// Perform a graceful shutdown sequence with fallback process kill.
    pub fn shutdown(&mut self, grace_period: Duration) -> Result<(), WorkerTerminalError> {
        if self.is_shut_down {
            return Ok(());
        }
        self.is_shut_down = true;

        let shutdown_env = Envelope {
            request_id: format!("shutdown-{}", Uuid::new_v4()),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: self.next_sequence(),
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::Shutdown(Shutdown {
                grace_period_ms: grace_period.as_millis() as u32,
            })),
        };

        let _ = write_frame(
            &mut self.writer,
            &shutdown_env,
            self.options.max_message_bytes,
        );

        // Wait for child to exit cleanly
        let start = Instant::now();
        if let Some(child) = self.child.as_mut() {
            while start.elapsed() < grace_period {
                match child.try_wait() {
                    Ok(Some(_status)) => return Ok(()),
                    Ok(None) => thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
            // Fallback forced termination
            let _ = child.kill();
            let _ = child.wait();
        }

        Ok(())
    }
}

impl Drop for CapabilityWorkerClient {
    fn drop(&mut self) {
        if !self.is_shut_down {
            let _ = self.shutdown(Duration::from_millis(500));
        }
    }
}

/// Generic capability worker activator.
pub struct CapabilityWorkerActivator;

impl CapabilityWorkerActivator {
    /// Activate a worker capability from a Platform `PluginManifest`.
    pub fn activate_from_manifest(
        manifest: &PluginManifest,
        options: &WorkerActivationOptions,
    ) -> Result<CapabilityWorkerClient, WorkerTerminalError> {
        let runtime = manifest.plugin.runtime.unwrap_or(Runtime::SubprocessPython);
        match runtime {
            Runtime::SubprocessPython => Self::activate_python_worker(manifest, options),
            Runtime::SubprocessJvm => Err(WorkerTerminalError::ActivationFailed(
                "SubprocessJvm runtime worker activation not yet supported".into(),
            )),
            Runtime::Service => Err(WorkerTerminalError::ActivationFailed(
                "Service runtime must use generic service supervision, not worker activator".into(),
            )),
        }
    }

    fn activate_python_worker(
        manifest: &PluginManifest,
        options: &WorkerActivationOptions,
    ) -> Result<CapabilityWorkerClient, WorkerTerminalError> {
        let python_bin = options
            .python_executable
            .clone()
            .or_else(|| std::env::var("CYRENE_PYTHON").ok())
            .or_else(|| {
                if let Some(wd) = &options.working_dir {
                    let mut curr = Some(wd.as_path());
                    while let Some(dir) = curr {
                        let venv_win = dir.join(".venv/Scripts/python.exe");
                        if venv_win.exists() {
                            return Some(venv_win.to_string_lossy().to_string());
                        }
                        let venv_unix = dir.join(".venv/bin/python");
                        if venv_unix.exists() {
                            return Some(venv_unix.to_string_lossy().to_string());
                        }
                        curr = dir.parent();
                    }
                }
                None
            })
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "python".to_string()
                } else {
                    "python3".to_string()
                }
            });

        let mut cmd = Command::new(&python_bin);
        cmd.args(["-m", "cyrene_worker_shim.runner"]);

        if let Some(entrypoint) = &manifest.plugin.entrypoint {
            cmd.args(["--entrypoint", entrypoint]);
        }
        cmd.args(["--plugin-id", &manifest.plugin.id]);
        cmd.args(["--plugin-version", &manifest.plugin.version]);
        for desc in &manifest.capability_descriptors {
            cmd.args(["--capability", &desc.id.id]);
        }

        if let Some(working_dir) = &options.working_dir {
            cmd.current_dir(working_dir);
        }

        // Build PYTHONPATH
        let mut python_paths = options.python_path.clone();
        if let Ok(existing) = std::env::var("PYTHONPATH") {
            python_paths.extend(std::env::split_paths(&existing));
        }
        if let Some(working_dir) = &options.working_dir {
            python_paths.push(working_dir.clone());
        }

        let python_path_str = std::env::join_paths(python_paths).map_err(|e| {
            WorkerTerminalError::ActivationFailed(format!("failed to construct PYTHONPATH: {e}"))
        })?;
        cmd.env("PYTHONPATH", python_path_str);
        cmd.env("PYTHONUNBUFFERED", "1");

        for (k, v) in &options.environment {
            cmd.env(k, v);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let child = cmd.spawn().map_err(|e| {
            WorkerTerminalError::WorkerUnavailable(format!(
                "failed to spawn python worker '{}': {e}",
                python_bin
            ))
        })?;

        let declared_caps = manifest
            .capability_descriptors
            .iter()
            .map(|d| d.id.id.clone())
            .collect::<Vec<_>>();

        let mut client = CapabilityWorkerClient::new(
            child,
            manifest.plugin.id.clone(),
            manifest.plugin.version.clone(),
            manifest.plugin.api_version.clone(),
            declared_caps,
            options.clone(),
        )?;

        // 3. Perform Handshake (Hello -> HelloAck)
        let hello_seq = client.next_sequence();
        let hello_req_id = format!("hello-{}", Uuid::new_v4());
        let hello_env = Envelope {
            request_id: hello_req_id.clone(),
            trace_id: String::new(),
            plugin_id: manifest.plugin.id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: hello_seq,
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        };

        write_frame(
            &mut client.writer,
            &hello_env,
            options.max_message_bytes,
        )?;

        let hello_ack = client
            .read_envelope_timed(options.handshake_timeout, &NeverCancelled)
            .map_err(|e| {
                WorkerTerminalError::ActivationFailed(format!("handshake failed: {e}"))
            })?;

        match hello_ack.payload {
            Some(Payload::HelloAck(ack)) => {
                if ack.selected_protocol_version != CURRENT_PROTOCOL_VERSION {
                    return Err(WorkerTerminalError::ProtocolMismatch(format!(
                        "worker selected unsupported protocol version {}",
                        ack.selected_protocol_version
                    )));
                }
                if !ack.plugin_id.is_empty() && ack.plugin_id != manifest.plugin.id {
                    return Err(WorkerTerminalError::ProtocolMismatch(format!(
                        "worker plugin ID mismatch: expected {}, got {}",
                        manifest.plugin.id, ack.plugin_id
                    )));
                }
            }
            Some(Payload::Error(err)) => {
                return Err(WorkerTerminalError::ProtocolMismatch(format!(
                    "worker rejected handshake: {}",
                    err.message
                )));
            }
            other => {
                return Err(WorkerTerminalError::ProtocolMismatch(format!(
                    "expected HelloAck, got {other:?}"
                )));
            }
        }

        Ok(client)
    }
}

/// Generic adapter that implements `MediaProcessor` on top of an active `CapabilityWorkerClient`.
pub struct WorkerMediaProcessor {
    client: Arc<Mutex<CapabilityWorkerClient>>,
}

impl WorkerMediaProcessor {
    pub fn new(client: CapabilityWorkerClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }
}

impl MediaProcessor for WorkerMediaProcessor {
    fn inspect_image(
        &self,
        request: &InspectImageRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<ImageInspection, MediaProcessorError> {
        let payload = serde_json::to_vec(request).map_err(|e| {
            MediaProcessorError::invalid_input(format!("failed to serialize request: {e}"))
        })?;

        let mut client = self.client.lock().map_err(|_| {
            MediaProcessorError::ExecutionFailed("worker client lock poisoned".into())
        })?;

        let timeout = client.options.default_invoke_timeout;
        let response_bytes = client
            .invoke("media.processor.v1", "inspect_image", &payload, timeout, cancellation)
            .map_err(map_worker_error_to_media)?;

        serde_json::from_slice::<ImageInspection>(&response_bytes).map_err(|e| {
            MediaProcessorError::ExecutionFailed(format!(
                "failed to deserialize ImageInspection response: {e}"
            ))
        })
    }

    fn transform_image(
        &self,
        request: &TransformImageRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<TransformedImage, MediaProcessorError> {
        request.validate()?;
        let payload = serde_json::to_vec(request).map_err(|e| {
            MediaProcessorError::invalid_input(format!("failed to serialize request: {e}"))
        })?;

        let mut client = self.client.lock().map_err(|_| {
            MediaProcessorError::ExecutionFailed("worker client lock poisoned".into())
        })?;

        let timeout = client.options.default_invoke_timeout;
        let response_bytes = client
            .invoke(
                "media.processor.v1",
                "transform_image",
                &payload,
                timeout,
                cancellation,
            )
            .map_err(map_worker_error_to_media)?;

        serde_json::from_slice::<TransformedImage>(&response_bytes).map_err(|e| {
            MediaProcessorError::ExecutionFailed(format!(
                "failed to deserialize TransformedImage response: {e}"
            ))
        })
    }
}

fn map_worker_error_to_media(err: WorkerTerminalError) -> MediaProcessorError {
    match err {
        WorkerTerminalError::Cancelled(_) => MediaProcessorError::Cancelled,
        WorkerTerminalError::InvalidRequest(msg) => {
            if msg.contains("UNSUPPORTED_INPUT") {
                MediaProcessorError::UnsupportedInput(msg)
            } else {
                MediaProcessorError::InvalidInput(msg)
            }
        }
        other => MediaProcessorError::ExecutionFailed(other.to_string()),
    }
}
