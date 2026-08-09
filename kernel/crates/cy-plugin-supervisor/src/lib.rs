use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use cy_local_transport::{LocalTransport, StdioTransport, TransportError};
use cy_plugin_protocol::pb::{
    envelope, plugin_error_payload, Cancel, Hello, Invoke, InvokeResult, Shutdown,
};
use cy_plugin_protocol::{Envelope, CURRENT_PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{info, warn};

/// Refined plugin runtime lifecycle states per specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginRuntimeState {
    Discovered,
    Resolved,
    Starting,
    Handshaking,
    Healthy,
    Degraded,
    Stopping,
    Stopped,
    Unavailable,
    Incompatible,
    Crashed,
    Quarantined,
    Disabled,
}

impl std::fmt::Display for PluginRuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub use cy_manifest::RestartPolicy;

/// Track crash history within sliding time window to detect crash loops.
#[derive(Debug, Clone)]
pub struct CrashTracker {
    history: VecDeque<Instant>,
    max_crashes_in_window: usize,
    window_duration: Duration,
}

impl CrashTracker {
    pub fn new(max_crashes_in_window: usize, window_duration: Duration) -> Self {
        Self {
            history: VecDeque::new(),
            max_crashes_in_window,
            window_duration,
        }
    }

    pub fn record_crash(&mut self) -> bool {
        let now = Instant::now();
        self.history.push_back(now);
        // Evict expired crash records
        while let Some(&t) = self.history.front() {
            if now.duration_since(t) > self.window_duration {
                self.history.pop_front();
            } else {
                break;
            }
        }
        self.history.len() >= self.max_crashes_in_window
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("Plugin launch failed: {0}")]
    LaunchFailed(String),
    #[error("Handshake timeout after {0:?}")]
    HandshakeTimeout(Duration),
    #[error("Incompatible protocol version: expected {expected}, got {got}")]
    IncompatibleProtocolVersion { expected: u32, got: u32 },
    #[error("Incompatible API version: {0}")]
    IncompatibleApiVersion(String),
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("Plugin is in quarantined state due to crash loop")]
    Quarantined,
    #[error("Plugin RPC error [{code:?}]: {message}")]
    PluginRpc {
        code: plugin_error_payload::Code,
        message: String,
    },
    #[error("Request timeout after {0:?}")]
    Timeout(Duration),
}

/// Plugin Supervisor managing process launch, zero-port transport, handshake, state tracking, and recovery.
pub struct PluginSupervisor {
    plugin_id: String,
    executable: String,
    args: Vec<String>,
    state: PluginRuntimeState,
    state_reason: Option<String>,
    restart_policy: RestartPolicy,
    crash_tracker: CrashTracker,
    restart_count: u32,
    transport_tx: Option<mpsc::Sender<Envelope>>,
    pending_requests: Arc<AsyncMutex<HashMap<String, oneshot::Sender<Envelope>>>>,
    declared_capabilities: Vec<String>,
    negotiated_api_version: Option<String>,
    pub capabilities_manifest: cy_manifest::PluginCapabilitiesManifest,
    pub metrics: HashMap<String, String>,
}

impl PluginSupervisor {
    pub fn new(
        plugin_id: impl Into<String>,
        executable: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            executable: executable.into(),
            args,
            state: PluginRuntimeState::Discovered,
            state_reason: None,
            restart_policy: RestartPolicy::default(),
            crash_tracker: CrashTracker::new(3, Duration::from_secs(60)),
            restart_count: 0,
            transport_tx: None,
            pending_requests: Arc::new(AsyncMutex::new(HashMap::new())),
            declared_capabilities: Vec::new(),
            negotiated_api_version: None,
            capabilities_manifest: Default::default(),
            metrics: HashMap::new(),
        }
    }

    pub fn state(&self) -> PluginRuntimeState {
        self.state
    }

    pub fn state_reason(&self) -> Option<&str> {
        self.state_reason.as_deref()
    }

    pub fn restart_policy(&self) -> &RestartPolicy {
        &self.restart_policy
    }

    pub fn set_restart_policy(&mut self, policy: RestartPolicy) {
        self.restart_policy = policy;
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn declared_capabilities(&self) -> &[String] {
        &self.declared_capabilities
    }

    pub fn unquarantine(&mut self) {
        if self.state == PluginRuntimeState::Quarantined {
            self.state = PluginRuntimeState::Stopped;
            self.state_reason = Some("Manually unquarantined".to_string());
            self.crash_tracker.reset();
        }
    }

    /// Ensure the plugin is healthy, attempting to restart it if allowed by the restart policy.
    pub async fn ensure_healthy(&mut self, timeout: Duration) -> Result<(), SupervisorError> {
        if self.state == PluginRuntimeState::Healthy {
            return Ok(());
        }
        if self.state == PluginRuntimeState::Quarantined {
            return Err(SupervisorError::Quarantined);
        }

        if self.state == PluginRuntimeState::Starting
            || self.state == PluginRuntimeState::Handshaking
        {
            return Err(SupervisorError::LaunchFailed(
                "Plugin is currently starting".to_string(),
            ));
        }

        if matches!(
            self.state,
            PluginRuntimeState::Discovered | PluginRuntimeState::Resolved
        ) {
            info!(
                "Plugin {} is in initial state ({:?}); starting...",
                self.plugin_id, self.state
            );
            self.start(timeout).await?;
            return Ok(());
        }

        if matches!(
            self.state,
            PluginRuntimeState::Crashed | PluginRuntimeState::Degraded
        ) {
            let should_restart = match &self.restart_policy {
                RestartPolicy::Never => false,
                RestartPolicy::Always => true,
                RestartPolicy::OnFailure { max_restarts, .. } => self.restart_count < *max_restarts,
            };

            if should_restart {
                self.restart_count += 1;
                let backoff = match &self.restart_policy {
                    RestartPolicy::OnFailure {
                        min_backoff_ms,
                        max_backoff_ms,
                        factor,
                        ..
                    } => {
                        let backoff = (*min_backoff_ms as f64
                            * factor.powi((self.restart_count - 1) as i32))
                            as u64;
                        Duration::from_millis(backoff.min(*max_backoff_ms))
                    }
                    RestartPolicy::Always => Duration::from_millis(1000),
                    _ => Duration::from_millis(0),
                };
                info!(
                    "Restarting plugin {}, attempt {}, backoff {:?}",
                    self.plugin_id, self.restart_count, backoff
                );
                tokio::time::sleep(backoff).await;

                // Clear old state
                self.transport_tx = None;
                self.pending_requests = Arc::new(AsyncMutex::new(HashMap::new()));

                self.start(timeout).await?;
                return Ok(());
            } else {
                return Err(SupervisorError::LaunchFailed(
                    "Plugin crashed and restart policy exhausted or disabled".to_string(),
                ));
            }
        }

        Err(SupervisorError::LaunchFailed(format!(
            "Plugin is in state {:?}",
            self.state
        )))
    }

    /// Start the plugin process, establish zero-port transport, and perform handshake.
    pub async fn start(&mut self, timeout: Duration) -> Result<(), SupervisorError> {
        if self.state == PluginRuntimeState::Quarantined {
            return Err(SupervisorError::Quarantined);
        }

        self.state = PluginRuntimeState::Starting;
        let arg_refs: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();

        let mut transport = match StdioTransport::spawn(&self.executable, &arg_refs) {
            Ok(t) => t,
            Err(e) => {
                self.state = PluginRuntimeState::Unavailable;
                self.state_reason = Some(format!("Failed to spawn executable: {e}"));
                return Err(SupervisorError::LaunchFailed(e.to_string()));
            }
        };

        self.state = PluginRuntimeState::Handshaking;

        // Construct Hello envelope
        let hello_req = Envelope {
            request_id: uuid::Uuid::new_v4().to_string(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: (Utc::now() + chrono::Duration::milliseconds(timeout.as_millis() as i64))
                .timestamp_millis(),
            sequence_number: 0,
            payload: Some(envelope::Payload::Hello(Hello {
                min_protocol_version: CURRENT_PROTOCOL_VERSION,
                max_protocol_version: CURRENT_PROTOCOL_VERSION,
                host_version: env!("CARGO_PKG_VERSION").to_string(),
            })),
        };

        transport.send(hello_req).await?;

        // Wait for HelloAck with timeout
        let ack_res = tokio::time::timeout(timeout, transport.receive()).await;
        match ack_res {
            Ok(Ok(Some(Envelope {
                payload: Some(envelope::Payload::HelloAck(ack)),
                ..
            }))) => {
                if ack.selected_protocol_version != CURRENT_PROTOCOL_VERSION {
                    self.state = PluginRuntimeState::Incompatible;
                    self.state_reason = Some(format!(
                        "Protocol version mismatch: host={}, plugin={}",
                        CURRENT_PROTOCOL_VERSION, ack.selected_protocol_version
                    ));
                    let _ = transport.close().await;
                    return Err(SupervisorError::IncompatibleProtocolVersion {
                        expected: CURRENT_PROTOCOL_VERSION,
                        got: ack.selected_protocol_version,
                    });
                }

                if !ack.plugin_id.is_empty() && ack.plugin_id != self.plugin_id {
                    self.state = PluginRuntimeState::Incompatible;
                    self.state_reason = Some(format!(
                        "Plugin identity mismatch: expected={}, got={}",
                        self.plugin_id, ack.plugin_id
                    ));
                    let _ = transport.close().await;
                    return Err(SupervisorError::LaunchFailed(format!(
                        "Plugin identity mismatch: expected={}, got={}",
                        self.plugin_id, ack.plugin_id
                    )));
                }

                if !ack.api_version.is_empty() && !ack.api_version.starts_with('1') {
                    self.state = PluginRuntimeState::Incompatible;
                    self.state_reason = Some(format!(
                        "Incompatible API version: host=1.0, plugin={}",
                        ack.api_version
                    ));
                    let _ = transport.close().await;
                    return Err(SupervisorError::LaunchFailed(format!(
                        "Incompatible API version: plugin reported {}",
                        ack.api_version
                    )));
                }

                self.declared_capabilities = ack.declared_capabilities;
                self.negotiated_api_version = Some(ack.api_version);
                self.metrics = ack.metrics;
                if !ack.capabilities_json.is_empty() {
                    if let Ok(manifest) = serde_json::from_str(&ack.capabilities_json) {
                        self.capabilities_manifest = manifest;
                    }
                }
                self.state = PluginRuntimeState::Healthy;
                self.state_reason = None;
                info!("Plugin {} launched and healthy.", self.plugin_id);

                // Spawn multiplexer background task
                let (tx, mut rx) = mpsc::channel::<Envelope>(100);
                self.transport_tx = Some(tx);
                let pending = self.pending_requests.clone();
                let plugin_id = self.plugin_id.clone();

                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            msg = rx.recv() => {
                                match msg {
                                    Some(env) => {
                                        if transport.send(env).await.is_err() {
                                            break;
                                        }
                                    }
                                    None => break, // tx dropped
                                }
                            }
                            res = transport.receive() => {
                                match res {
                                    Ok(Some(env)) => {
                                        let mut map = pending.lock().await;
                                        if let Some(sender) = map.remove(&env.request_id) {
                                            let _ = sender.send(env);
                                        } else {
                                            warn!("Late or unknown response received for request_id: {} from plugin {}", env.request_id, plugin_id);
                                        }
                                    }
                                    _ => break, // EOF or error
                                }
                            }
                        }
                    }
                    let _ = transport.close().await;
                });

                Ok(())
            }
            Ok(Ok(Some(Envelope {
                payload: Some(envelope::Payload::Error(err)),
                ..
            }))) => {
                self.state = PluginRuntimeState::Unavailable;
                self.state_reason = Some(format!("Handshake rejected by plugin: {}", err.message));
                let _ = transport.close().await;
                Err(SupervisorError::PluginRpc {
                    code: plugin_error_payload::Code::try_from(err.code)
                        .unwrap_or(plugin_error_payload::Code::Unknown),
                    message: err.message,
                })
            }
            Ok(Ok(None)) | Ok(Ok(Some(_))) => {
                self.state = PluginRuntimeState::Crashed;
                self.state_reason = Some("Process closed stream during handshake".to_string());
                let _ = transport.close().await;
                Err(SupervisorError::LaunchFailed(
                    "Unexpected stream EOF during handshake".to_string(),
                ))
            }
            Ok(Err(e)) => {
                self.state = PluginRuntimeState::Crashed;
                self.state_reason = Some(format!("Transport error during handshake: {e}"));
                let _ = transport.close().await;
                Err(SupervisorError::Transport(e))
            }
            Err(_) => {
                self.state = PluginRuntimeState::Quarantined;
                self.state_reason = Some("Handshake timeout".to_string());
                let _ = transport.close().await;
                Err(SupervisorError::HandshakeTimeout(timeout))
            }
        }
    }

    /// Invoke an extension point RPC on the plugin.
    pub async fn invoke(
        &mut self,
        invoke_payload: Invoke,
        timeout: Duration,
    ) -> Result<InvokeResult, SupervisorError> {
        self.ensure_healthy(Duration::from_secs(10)).await?;

        let tx = self
            .transport_tx
            .as_ref()
            .ok_or_else(|| SupervisorError::LaunchFailed("Transport not initialized".to_string()))?
            .clone();

        let req_id = uuid::Uuid::new_v4().to_string();
        let env = Envelope {
            request_id: req_id.clone(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            plugin_id: self.plugin_id.clone(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: (Utc::now() + chrono::Duration::milliseconds(timeout.as_millis() as i64))
                .timestamp_millis(),
            sequence_number: 0,
            payload: Some(envelope::Payload::Invoke(invoke_payload)),
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending_requests
            .lock()
            .await
            .insert(req_id.clone(), reply_tx);

        if tx.send(env).await.is_err() {
            self.pending_requests.lock().await.remove(&req_id);
            warn!("Failed to route request to plugin {}", self.plugin_id);
            self.handle_crash();
            return Err(SupervisorError::Transport(TransportError::Closed));
        }

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(Envelope {
                payload: Some(envelope::Payload::InvokeResult(res)),
                ..
            })) => Ok(res),
            Ok(Ok(Envelope {
                payload: Some(envelope::Payload::Error(err)),
                ..
            })) => Err(SupervisorError::PluginRpc {
                code: plugin_error_payload::Code::try_from(err.code)
                    .unwrap_or(plugin_error_payload::Code::Unknown),
                message: err.message,
            }),
            Ok(Ok(_)) | Ok(Err(_)) => {
                self.handle_crash();
                Err(SupervisorError::LaunchFailed(
                    "Plugin process crashed or closed connection during invoke".to_string(),
                ))
            }
            Err(_) => {
                // Send Cancel message on timeout, remove oneshot to prevent crossed lines
                self.pending_requests.lock().await.remove(&req_id);
                let cancel_env = Envelope {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    trace_id: uuid::Uuid::new_v4().to_string(),
                    plugin_id: self.plugin_id.clone(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: 0,
                    payload: Some(envelope::Payload::Cancel(Cancel {
                        target_request_id: req_id,
                        reason: "Invoke timeout".to_string(),
                    })),
                };
                let _ = tx.send(cancel_env).await;
                self.handle_crash();
                Err(SupervisorError::Timeout(timeout))
            }
        }
    }

    fn handle_crash(&mut self) {
        let is_crash_loop = self.crash_tracker.record_crash();
        if is_crash_loop {
            self.state = PluginRuntimeState::Quarantined;
            self.state_reason =
                Some("Quarantined: repeated crashes in short time window".to_string());
            warn!("Plugin {} quarantined due to crash loop.", self.plugin_id);
        } else {
            self.state = PluginRuntimeState::Crashed;
            self.state_reason =
                Some("Process exited or stream terminated unexpectedly".to_string());
        }
        self.transport_tx = None;
    }

    /// Stop the plugin process gracefully.
    pub async fn stop(&mut self) -> Result<(), SupervisorError> {
        self.state = PluginRuntimeState::Stopping;
        if let Some(tx) = self.transport_tx.take() {
            let shutdown_env = Envelope {
                request_id: uuid::Uuid::new_v4().to_string(),
                trace_id: uuid::Uuid::new_v4().to_string(),
                plugin_id: self.plugin_id.clone(),
                protocol_version: CURRENT_PROTOCOL_VERSION,
                deadline_ms: 1000,
                sequence_number: 0,
                payload: Some(envelope::Payload::Shutdown(Shutdown {
                    grace_period_ms: 1000,
                })),
            };
            let _ = tx.send(shutdown_env).await;
        }
        self.state = PluginRuntimeState::Stopped;
        Ok(())
    }
}
