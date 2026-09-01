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
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use cy_manifest::{PluginManifest, Runtime};
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{
        ApplicationEvent, ApplicationEventStreamEnd, Cancel, Envelope, Hello, Invoke,
        PluginErrorPayload, Shutdown, Subscribe,
    },
    plugin_error_payload, CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_MESSAGE_BYTES,
};
use prost::Message;
use thiserror::Error;
use uuid::Uuid;

use crate::media::{
    ImageInspection, InspectImageRequest, MediaProcessor, MediaProcessorError,
    NormalizeAudioRequest, NormalizedAudio, TransformImageRequest, TransformedImage,
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

/// Default and maximum number of application events retained by one live
/// subscription. The caller may request a smaller bounded buffer.
pub const DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY: usize = 32;
pub const MAX_APPLICATION_EVENT_BUFFER_CAPACITY: usize = 1024;

/// Product-neutral application data delivered by a capability worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerApplicationEvent {
    pub subscription_id: String,
    pub capability: String,
    pub event_sequence: u64,
    pub event_type: String,
    pub payload: Vec<u8>,
    pub payload_type_url: String,
    pub generation: u64,
    pub source_id: String,
}

/// Product-neutral result returned by one worker invocation.
///
/// The worker owns the payload schema. The optional type URL is only transport
/// metadata that lets the CES preserve a capability-owned protobuf `Any`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerInvocationResult {
    pub payload: Vec<u8>,
    pub payload_type_url: String,
}

/// Terminal condition for one application-event subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationEventStreamEndReason {
    NormalCompletion,
    Cancelled,
    WorkerUnavailable,
    WorkerCrash,
    ProtocolFailure,
    GenerationTerminated,
    Backpressure,
}

impl ApplicationEventStreamEndReason {
    fn from_wire(value: i32) -> Self {
        match value {
            0 => Self::NormalCompletion,
            1 => Self::Cancelled,
            2 => Self::WorkerUnavailable,
            3 => Self::WorkerCrash,
            4 => Self::ProtocolFailure,
            5 => Self::GenerationTerminated,
            6 => Self::Backpressure,
            _ => Self::ProtocolFailure,
        }
    }
}

/// Observable terminal metadata for an application-event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationEventStreamTermination {
    pub subscription_id: String,
    pub capability: String,
    pub generation: u64,
    pub source_id: String,
    pub reason: ApplicationEventStreamEndReason,
    pub message: String,
}

/// Result of reading one live application event.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApplicationEventError {
    #[error("application-event stream read timed out after {0}ms")]
    Timeout(u128),
    #[error("application-event stream terminated: {0:?}")]
    Terminated(ApplicationEventStreamTermination),
}

impl ApplicationEventError {
    pub fn termination(&self) -> Option<&ApplicationEventStreamTermination> {
        match self {
            Self::Terminated(termination) => Some(termination),
            Self::Timeout(_) => None,
        }
    }
}

const INITIAL_WORKER_GENERATION: u64 = 1;
const INITIAL_WORKER_FENCE_TOKEN: u64 = 1;
const CONTROL_RESPONSE_BUFFER_CAPACITY: usize = 64;

struct WorkerTransport {
    writer: Mutex<BufWriter<ChildStdin>>,
    sequence_number: AtomicU64,
    plugin_id: String,
    max_message_bytes: usize,
    generation: u64,
    fence_token: u64,
}

impl WorkerTransport {
    fn next_sequence(&self) -> u64 {
        self.sequence_number.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn send(&self, envelope: &Envelope) -> Result<(), WorkerTerminalError> {
        let mut writer = self.writer.lock().map_err(|_| {
            WorkerTerminalError::WorkerUnavailable("worker writer lock poisoned".into())
        })?;
        write_frame(&mut *writer, envelope, self.max_message_bytes)
    }

    fn send_cancel_best_effort(&self, target_request_id: &str) {
        let envelope = Envelope {
            request_id: format!("cancel-{target_request_id}"),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: self.next_sequence(),
            generation: self.generation,
            fence_token: self.fence_token,
            payload: Some(Payload::Cancel(Cancel {
                target_request_id: target_request_id.to_string(),
                reason: "application-event buffer backpressure".to_string(),
            })),
        };
        let _ = self.send(&envelope);
    }
}

struct EventSubscriptionState {
    sender: SyncSender<Result<WorkerApplicationEvent, ApplicationEventStreamTermination>>,
    queued_events: Arc<AtomicUsize>,
    buffer_capacity: usize,
    capability: String,
    generation: u64,
    fence_token: u64,
    last_event_sequence: u64,
}

struct EventRegistry {
    subscriptions: Mutex<HashMap<String, EventSubscriptionState>>,
    plugin_id: String,
    generation: u64,
    fence_token: u64,
}

impl EventRegistry {
    fn new(plugin_id: String, generation: u64, fence_token: u64) -> Self {
        Self {
            subscriptions: Mutex::new(HashMap::new()),
            plugin_id,
            generation,
            fence_token,
        }
    }

    fn register(
        &self,
        subscription_id: String,
        capability: String,
        buffer_capacity: usize,
    ) -> Result<
        (
            Receiver<Result<WorkerApplicationEvent, ApplicationEventStreamTermination>>,
            Arc<AtomicUsize>,
        ),
        WorkerTerminalError,
    > {
        let (sender, receiver) = mpsc::sync_channel(buffer_capacity + 1);
        let queued_events = Arc::new(AtomicUsize::new(0));
        let state = EventSubscriptionState {
            sender,
            queued_events: queued_events.clone(),
            buffer_capacity,
            capability,
            generation: self.generation,
            fence_token: self.fence_token,
            last_event_sequence: 0,
        };
        let mut subscriptions = self.subscriptions.lock().map_err(|_| {
            WorkerTerminalError::WorkerUnavailable("event registry lock poisoned".into())
        })?;
        if subscriptions.insert(subscription_id.clone(), state).is_some() {
            return Err(WorkerTerminalError::ProtocolMismatch(format!(
                "duplicate application-event subscription {subscription_id}"
            )));
        }
        Ok((receiver, queued_events))
    }

    fn remove(&self, subscription_id: &str) {
        if let Ok(mut subscriptions) = self.subscriptions.lock() {
            subscriptions.remove(subscription_id);
        }
    }

    fn contains(&self, subscription_id: &str) -> bool {
        self.subscriptions
            .lock()
            .map(|subscriptions| subscriptions.contains_key(subscription_id))
            .unwrap_or(false)
    }

    fn termination(
        &self,
        subscription_id: &str,
        capability: &str,
        generation: u64,
        source_id: &str,
        reason: ApplicationEventStreamEndReason,
        message: impl Into<String>,
    ) -> ApplicationEventStreamTermination {
        ApplicationEventStreamTermination {
            subscription_id: subscription_id.to_string(),
            capability: capability.to_string(),
            generation,
            source_id: source_id.to_string(),
            reason,
            message: message.into(),
        }
    }

    fn terminate_all(&self, reason: ApplicationEventStreamEndReason, message: impl Into<String>) {
        let message = message.into();
        let states = self
            .subscriptions
            .lock()
            .ok()
            .map(|mut subscriptions| std::mem::take(&mut *subscriptions))
            .unwrap_or_default();
        for (subscription_id, state) in states {
            let termination = self.termination(
                &subscription_id,
                &state.capability,
                self.generation,
                &self.plugin_id,
                reason,
                message.clone(),
            );
            let _ = state.sender.try_send(Err(termination));
        }
    }

    fn route(&self, envelope: Envelope) -> (bool, Option<String>) {
        match envelope.payload.clone() {
            Some(Payload::ApplicationEvent(event)) => {
                (true, self.route_event(envelope, event))
            }
            Some(Payload::ApplicationEventStreamEnd(end)) => {
                self.route_end(envelope, end);
                (true, None)
            }
            _ => (false, None),
        }
    }

    fn route_event(&self, envelope: Envelope, event: ApplicationEvent) -> Option<String> {
        let subscription_id = event.subscription_id.clone();
        let mut termination = None;
        let mut terminal_sender = None;
        let mut remove_disconnected = false;
        let mut cancel_subscription_id = None;
        if let Ok(mut subscriptions) = self.subscriptions.lock() {
            if let Some(state) = subscriptions.get_mut(&subscription_id) {
                let source_id = if envelope.plugin_id.is_empty() {
                    self.plugin_id.clone()
                } else {
                    envelope.plugin_id.clone()
                };
                if source_id != self.plugin_id {
                    termination = Some(self.termination(
                        &subscription_id,
                        &state.capability,
                        envelope.generation,
                        &source_id,
                        ApplicationEventStreamEndReason::ProtocolFailure,
                        "application-event source identity mismatch",
                    ));
                } else if event.capability != state.capability {
                    termination = Some(self.termination(
                        &subscription_id,
                        &state.capability,
                        envelope.generation,
                        &source_id,
                        ApplicationEventStreamEndReason::ProtocolFailure,
                        "application-event capability mismatch",
                    ));
                } else if envelope.generation != state.generation
                    || envelope.fence_token != state.fence_token
                {
                    let reason = if envelope.generation > state.generation
                        || envelope.fence_token > state.fence_token
                    {
                        ApplicationEventStreamEndReason::GenerationTerminated
                    } else {
                        ApplicationEventStreamEndReason::ProtocolFailure
                    };
                    termination = Some(self.termination(
                        &subscription_id,
                        &state.capability,
                        envelope.generation,
                        &source_id,
                        reason,
                        "application-event generation or fence changed",
                    ));
                } else if event.event_sequence != state.last_event_sequence + 1 {
                    termination = Some(self.termination(
                        &subscription_id,
                        &state.capability,
                        envelope.generation,
                        &source_id,
                        ApplicationEventStreamEndReason::ProtocolFailure,
                        "application-event sequence is not contiguous",
                    ));
                } else if state.queued_events.load(Ordering::Acquire) >= state.buffer_capacity {
                    termination = Some(self.termination(
                        &subscription_id,
                        &state.capability,
                        envelope.generation,
                        &source_id,
                        ApplicationEventStreamEndReason::Backpressure,
                        format!(
                            "application-event buffer reached {} events",
                            state.buffer_capacity
                        ),
                    ));
                    cancel_subscription_id = Some(subscription_id.clone());
                } else {
                    let application_event = WorkerApplicationEvent {
                        subscription_id: subscription_id.clone(),
                        capability: event.capability,
                        event_sequence: event.event_sequence,
                        event_type: event.event_type,
                        payload: event.payload,
                        payload_type_url: event.payload_type_url,
                        generation: envelope.generation,
                        source_id,
                    };
                    state.queued_events.fetch_add(1, Ordering::AcqRel);
                    match state.sender.try_send(Ok(application_event)) {
                        Ok(()) => state.last_event_sequence = event.event_sequence,
                        Err(TrySendError::Full(_)) => {
                            state.queued_events.fetch_sub(1, Ordering::AcqRel);
                            termination = Some(self.termination(
                                &subscription_id,
                                &state.capability,
                                envelope.generation,
                                &self.plugin_id,
                                ApplicationEventStreamEndReason::Backpressure,
                                "application-event buffer is full",
                            ));
                            cancel_subscription_id = Some(subscription_id.clone());
                        }
                        Err(TrySendError::Disconnected(_)) => remove_disconnected = true,
                    }
                }
            }
            if termination.is_some() || remove_disconnected {
                if termination.is_some() {
                    terminal_sender = subscriptions
                        .get(&subscription_id)
                        .map(|state| state.sender.clone());
                }
                subscriptions.remove(&subscription_id);
            }
        }
        if let (Some(termination), Some(sender)) = (termination, terminal_sender) {
            let _ = sender.try_send(Err(termination));
        }
        cancel_subscription_id
    }

    fn route_end(&self, envelope: Envelope, end: ApplicationEventStreamEnd) {
        let subscription_id = end.subscription_id.clone();
        let state = self
            .subscriptions
            .lock()
            .ok()
            .and_then(|mut subscriptions| subscriptions.remove(&subscription_id));
        let Some(state) = state else {
            return;
        };
        let source_id = if envelope.plugin_id.is_empty() {
            self.plugin_id.clone()
        } else {
            envelope.plugin_id.clone()
        };
        let (reason, message) = if source_id != self.plugin_id
            || end.capability != state.capability
            || envelope.generation < state.generation
            || envelope.fence_token < state.fence_token
        {
            (
                ApplicationEventStreamEndReason::ProtocolFailure,
                "application-event terminal metadata mismatch".to_string(),
            )
        } else {
            (
                ApplicationEventStreamEndReason::from_wire(end.reason),
                end.message,
            )
        };
        let termination = self.termination(
            &subscription_id,
            &state.capability,
            envelope.generation,
            &source_id,
            reason,
            message,
        );
        let _ = state.sender.try_send(Err(termination));
    }
}

pub struct ApplicationEventSubscription {
    subscription_id: String,
    capability: String,
    receiver: Receiver<Result<WorkerApplicationEvent, ApplicationEventStreamTermination>>,
    queued_events: Arc<AtomicUsize>,
    registry: Arc<EventRegistry>,
    transport: Arc<WorkerTransport>,
    terminal: Mutex<Option<ApplicationEventStreamTermination>>,
    cancel_requested: AtomicBool,
}

impl ApplicationEventSubscription {
    pub fn subscription_id(&self) -> &str {
        &self.subscription_id
    }

    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Read the next event. Every terminal condition is returned as a typed
    /// stream termination; no terminal state is silently converted to EOF.
    pub fn next(&self, timeout: Duration) -> Result<WorkerApplicationEvent, ApplicationEventError> {
        if let Ok(terminal) = self.terminal.lock() {
            if let Some(terminal) = terminal.clone() {
                return Err(ApplicationEventError::Terminated(terminal));
            }
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                self.queued_events.fetch_sub(1, Ordering::AcqRel);
                Ok(event)
            }
            Ok(Err(terminal)) => {
                self.cancel_requested.store(true, Ordering::SeqCst);
                if let Ok(mut current) = self.terminal.lock() {
                    *current = Some(terminal.clone());
                }
                Err(ApplicationEventError::Terminated(terminal))
            }
            Err(RecvTimeoutError::Timeout) => {
                Err(ApplicationEventError::Timeout(timeout.as_millis()))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.cancel_requested.store(true, Ordering::SeqCst);
                let terminal = ApplicationEventStreamTermination {
                    subscription_id: self.subscription_id.clone(),
                    capability: self.capability.clone(),
                    generation: self.transport.generation,
                    source_id: self.transport.plugin_id.clone(),
                    reason: ApplicationEventStreamEndReason::WorkerUnavailable,
                    message: "application-event subscription channel disconnected".into(),
                };
                if let Ok(mut current) = self.terminal.lock() {
                    *current = Some(terminal.clone());
                }
                Err(ApplicationEventError::Terminated(terminal))
            }
        }
    }

    /// Send the canonical Cancel operation and wait for its CancelAck.
    pub fn unsubscribe(
        &self,
        client: &mut CapabilityWorkerClient,
        timeout: Duration,
    ) -> Result<(), WorkerTerminalError> {
        let result = client.unsubscribe_application_events(&self.subscription_id, timeout);
        if result.is_ok() {
            self.cancel_requested.store(true, Ordering::SeqCst);
        }
        result
    }
}

impl Drop for ApplicationEventSubscription {
    fn drop(&mut self) {
        if !self.cancel_requested.swap(true, Ordering::SeqCst) {
            self.registry.remove(&self.subscription_id);
            self.transport.send_cancel_best_effort(&self.subscription_id);
        }
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
    transport: Arc<WorkerTransport>,
    reader_rx: Receiver<Result<Envelope, WorkerTerminalError>>,
    event_registry: Arc<EventRegistry>,
    lifecycle: Arc<AtomicBool>,
    plugin_id: String,
    plugin_version: String,
    api_version: String,
    declared_capabilities: Vec<String>,
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
        let transport = Arc::new(WorkerTransport {
            writer: Mutex::new(BufWriter::new(stdin)),
            sequence_number: AtomicU64::new(0),
            plugin_id: plugin_id.clone(),
            max_message_bytes: max_bytes,
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
        });
        let event_registry = Arc::new(EventRegistry::new(
            plugin_id.clone(),
            INITIAL_WORKER_GENERATION,
            INITIAL_WORKER_FENCE_TOKEN,
        ));
        let lifecycle = Arc::new(AtomicBool::new(false));
        let (reader_tx, reader_rx) = mpsc::sync_channel(CONTROL_RESPONSE_BUFFER_CAPACITY);
        let reader_registry = Arc::clone(&event_registry);
        let reader_transport = Arc::clone(&transport);
        let reader_lifecycle = Arc::clone(&lifecycle);
        let reader_plugin_id = plugin_id.clone();
        thread::Builder::new()
            .name(format!("worker-reader-{}", plugin_id))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_frame(&mut reader, max_bytes) {
                        Ok(Some(env)) => {
                            let (routed, cancel_subscription_id) = reader_registry.route(env.clone());
                            if let Some(subscription_id) = cancel_subscription_id {
                                reader_transport.send_cancel_best_effort(&subscription_id);
                            }
                            if routed {
                                continue;
                            }
                            if reader_tx.try_send(Ok(env)).is_err() {
                                reader_registry.terminate_all(
                                    ApplicationEventStreamEndReason::WorkerUnavailable,
                                    "bounded control-response buffer is full",
                                );
                                break;
                            }
                        }
                        Ok(None) => {
                            let reason = if reader_lifecycle.load(Ordering::SeqCst) {
                                ApplicationEventStreamEndReason::NormalCompletion
                            } else {
                                ApplicationEventStreamEndReason::WorkerCrash
                            };
                            reader_registry.terminate_all(
                                reason,
                                "worker process closed stream (EOF)",
                            );
                            let _ = reader_tx.try_send(Err(WorkerTerminalError::WorkerCrashed(
                                format!("worker '{}' closed stream (EOF)", reader_plugin_id),
                            )));
                            break;
                        }
                        Err(err) => {
                            reader_registry.terminate_all(
                                ApplicationEventStreamEndReason::ProtocolFailure,
                                err.to_string(),
                            );
                            let _ = reader_tx.try_send(Err(err));
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
            transport,
            reader_rx,
            event_registry,
            lifecycle,
            plugin_id,
            plugin_version,
            api_version,
            declared_capabilities,
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

    /// Poll the owned child process without inferring runtime state from
    /// package or binding metadata.
    pub fn is_running(&mut self) -> Result<bool, WorkerTerminalError> {
        if self.is_shut_down {
            return Ok(false);
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(false);
        };
        child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| {
                WorkerTerminalError::WorkerUnavailable(format!(
                    "failed to poll worker process: {error}"
                ))
            })
    }

    fn next_sequence(&self) -> u64 {
        self.transport.next_sequence()
    }

    fn write_envelope(&self, envelope: &Envelope) -> Result<(), WorkerTerminalError> {
        self.transport.send(envelope)
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

    /// Start a live application-event subscription using the default bounded
    /// buffer. This is a capability data-plane operation, not Kernel event
    /// subscription.
    pub fn subscribe(
        &mut self,
        capability: &str,
        filter_payload: &[u8],
        timeout: Duration,
    ) -> Result<ApplicationEventSubscription, WorkerTerminalError> {
        self.subscribe_application_events_with_buffer(
            capability,
            filter_payload,
            DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY,
            timeout,
        )
    }

    pub fn subscribe_application_events(
        &mut self,
        capability: &str,
        filter_payload: &[u8],
        timeout: Duration,
    ) -> Result<ApplicationEventSubscription, WorkerTerminalError> {
        self.subscribe(capability, filter_payload, timeout)
    }

    pub fn subscribe_application_events_with_buffer(
        &mut self,
        capability: &str,
        filter_payload: &[u8],
        buffer_capacity: usize,
        timeout: Duration,
    ) -> Result<ApplicationEventSubscription, WorkerTerminalError> {
        self.subscribe_application_events_with_cancellation(
            capability,
            filter_payload,
            buffer_capacity,
            timeout,
            &NeverCancelled,
        )
    }

    pub fn subscribe_application_events_with_cancellation(
        &mut self,
        capability: &str,
        filter_payload: &[u8],
        buffer_capacity: usize,
        timeout: Duration,
        cancellation: &dyn CancellationToken,
    ) -> Result<ApplicationEventSubscription, WorkerTerminalError> {
        if self.is_shut_down {
            return Err(WorkerTerminalError::WorkerUnavailable(
                "worker client is already shut down".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(WorkerTerminalError::Cancelled(
                "pre-subscription cancellation".into(),
            ));
        }
        if capability.is_empty() {
            return Err(WorkerTerminalError::InvalidRequest(
                "application-event capability is required".into(),
            ));
        }
        if !(1..=MAX_APPLICATION_EVENT_BUFFER_CAPACITY).contains(&buffer_capacity) {
            return Err(WorkerTerminalError::InvalidRequest(format!(
                "application-event buffer capacity must be between 1 and {}",
                MAX_APPLICATION_EVENT_BUFFER_CAPACITY
            )));
        }

        let subscription_id = Uuid::new_v4().to_string();
        let (receiver, queued_events) = self.event_registry.register(
            subscription_id.clone(),
            capability.to_string(),
            buffer_capacity,
        )?;
        let envelope = Envelope {
            request_id: subscription_id.clone(),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: self.next_sequence(),
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
            payload: Some(Payload::Subscribe(Subscribe {
                capability: capability.to_string(),
                filter_payload: filter_payload.to_vec(),
                max_buffered_events: buffer_capacity as u32,
            })),
        };

        if let Err(error) = self.write_envelope(&envelope) {
            self.event_registry.remove(&subscription_id);
            return Err(error);
        }

        let response = match self.read_envelope_timed(timeout, cancellation) {
            Ok(response) => response,
            Err(error) => {
                self.event_registry.remove(&subscription_id);
                return Err(error);
            }
        };
        if response.request_id != subscription_id {
            self.event_registry.remove(&subscription_id);
            return Err(WorkerTerminalError::ProtocolMismatch(format!(
                "expected SubscribeAck for {subscription_id}, got {}",
                response.request_id
            )));
        }

        match response.payload {
            Some(Payload::SubscribeAck(ack))
                if ack.subscription_id == subscription_id && ack.capability == capability =>
            {
                Ok(ApplicationEventSubscription {
                    subscription_id,
                    capability: capability.to_string(),
                    receiver,
                    queued_events,
                    registry: Arc::clone(&self.event_registry),
                    transport: Arc::clone(&self.transport),
                    terminal: Mutex::new(None),
                    cancel_requested: AtomicBool::new(false),
                })
            }
            Some(Payload::Error(error)) => {
                self.event_registry.remove(&subscription_id);
                Err(Self::map_error_payload(error))
            }
            other => {
                self.event_registry.remove(&subscription_id);
                Err(WorkerTerminalError::ProtocolMismatch(format!(
                    "expected SubscribeAck, got {other:?}"
                )))
            }
        }
    }

    /// Send Cancel for a subscription and wait only for CancelAck. The
    /// authoritative stream-end frame is still required and is delivered by
    /// ApplicationEventSubscription::next.
    pub fn unsubscribe_application_events(
        &mut self,
        subscription_id: &str,
        timeout: Duration,
    ) -> Result<(), WorkerTerminalError> {
        if !self.event_registry.contains(subscription_id) {
            return Ok(());
        }
        let request_id = format!("cancel-{}", Uuid::new_v4());
        let envelope = Envelope {
            request_id: request_id.clone(),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: self.next_sequence(),
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
            payload: Some(Payload::Cancel(Cancel {
                target_request_id: subscription_id.to_string(),
                reason: "caller unsubscribed".to_string(),
            })),
        };
        self.write_envelope(&envelope)?;
        let response = self.read_envelope_timed(timeout, &NeverCancelled)?;
        if response.request_id != request_id {
            return Err(WorkerTerminalError::ProtocolMismatch(format!(
                "expected CancelAck for {request_id}, got {}",
                response.request_id
            )));
        }
        match response.payload {
            Some(Payload::CancelAck(ack)) if ack.target_request_id == subscription_id => Ok(()),
            Some(Payload::Error(error)) => Err(Self::map_error_payload(error)),
            other => Err(WorkerTerminalError::ProtocolMismatch(format!(
                "expected CancelAck, got {other:?}"
            ))),
        }
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
        self.invoke_typed(capability, method, payload, timeout, cancellation)
            .map(|result| result.payload)
    }

    /// Invoke a capability and preserve optional worker-owned payload type
    /// metadata for the Product-facing CES `Any` response.
    pub fn invoke_typed(
        &mut self,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
        cancellation: &dyn CancellationToken,
    ) -> Result<WorkerInvocationResult, WorkerTerminalError> {
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
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
            payload: Some(Payload::Invoke(Invoke {
                extension_point: capability.to_string(),
                method: method.to_string(),
                payload: payload.to_vec(),
                request: None,
            })),
        };

        self.write_envelope(&invoke_env)?;

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
                    generation: INITIAL_WORKER_GENERATION,
                    fence_token: INITIAL_WORKER_FENCE_TOKEN,
                    payload: Some(Payload::Cancel(Cancel {
                        target_request_id: request_id.clone(),
                        reason: "caller requested cancellation".to_string(),
                    })),
                };
                let _ = self.write_envelope(&cancel_env);

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
                        Some(Payload::InvokeResult(result)) => {
                            return Ok(WorkerInvocationResult {
                                payload: result.payload,
                                payload_type_url: result.payload_type_url,
                            })
                        }
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
        self.lifecycle.store(true, Ordering::SeqCst);

        let shutdown_env = Envelope {
            request_id: format!("shutdown-{}", Uuid::new_v4()),
            trace_id: String::new(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: self.next_sequence(),
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
            payload: Some(Payload::Shutdown(Shutdown {
                grace_period_ms: grace_period.as_millis() as u32,
            })),
        };

        let _ = self.write_envelope(&shutdown_env);

        // Wait for child to exit cleanly
        let start = Instant::now();
        if let Some(child) = self.child.as_mut() {
            while start.elapsed() < grace_period {
                match child.try_wait() {
                    Ok(Some(_status)) => {
                        self.event_registry.terminate_all(
                            ApplicationEventStreamEndReason::NormalCompletion,
                            "worker shutdown",
                        );
                        return Ok(());
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
            // Fallback forced termination
            let _ = child.kill();
            let _ = child.wait();
        }

        self.event_registry.terminate_all(
            ApplicationEventStreamEndReason::NormalCompletion,
            "worker shutdown",
        );

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
            generation: INITIAL_WORKER_GENERATION,
            fence_token: INITIAL_WORKER_FENCE_TOKEN,
            payload: Some(Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        };

        client.write_envelope(&hello_env)?;

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

    fn normalize_audio(
        &self,
        request: &NormalizeAudioRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<NormalizedAudio, MediaProcessorError> {
        let payload = serde_json::to_vec(request).map_err(|error| {
            MediaProcessorError::invalid_input(format!("failed to serialize request: {error}"))
        })?;
        let mut client = self.client.lock().map_err(|_| {
            MediaProcessorError::ExecutionFailed("worker client lock poisoned".into())
        })?;
        let timeout = client.options.default_invoke_timeout;
        let response_bytes = client
            .invoke(
                "media.processor.v1",
                "normalize_audio",
                &payload,
                timeout,
                cancellation,
            )
            .map_err(map_worker_error_to_media)?;
        serde_json::from_slice::<NormalizedAudio>(&response_bytes).map_err(|error| {
            MediaProcessorError::ExecutionFailed(format!(
                "failed to deserialize NormalizedAudio response: {error}"
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
