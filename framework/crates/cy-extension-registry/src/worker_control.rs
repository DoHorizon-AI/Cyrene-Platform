// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/worker_control.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic worker-control client for the framework-owned worker channel.
//!
//! This module knows only the worker envelope and lifecycle messages. Product
//! adapters own the meaning of configuration keys and capability payloads.

use std::{collections::HashMap, time::Duration};

use async_trait::async_trait;
use cy_kernel_api::ProviderError;
use cy_kernel_daemon::watchdog::InstanceActor;
use cy_plugin_protocol::{
    envelope::Payload,
    pb::{Cancel, CancelAck, Configure, Envelope, Hello, HelloAck, Invoke, InvokeResult},
    CURRENT_PROTOCOL_VERSION,
};
use prost::Message;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerControlError {
    #[error("worker transport error: {0}")]
    Transport(#[from] ProviderError),
    #[error("worker control request timed out")]
    Timeout,
    #[error("worker protocol error: {0}")]
    Protocol(String),
    #[error("worker response fence mismatch: expected generation={expected_generation}/fence={expected_fence}, got generation={actual_generation}/fence={actual_fence}")]
    FenceMismatch {
        expected_generation: u64,
        expected_fence: u64,
        actual_generation: u64,
        actual_fence: u64,
    },
    #[error("worker returned an error: {0}")]
    Remote(String),
}

/// Replaceable port for a generic worker-control transport.
#[async_trait]
pub trait WorkerControlPort {
    async fn send(
        &mut self,
        envelope: Envelope,
        timeout: Duration,
    ) -> Result<Envelope, WorkerControlError>;
}

/// Framework adapter from an [`InstanceActor`] to the generic worker protocol.
pub struct WorkerControlClient<'a> {
    actor: &'a mut InstanceActor,
    plugin_id: String,
}

impl<'a> WorkerControlClient<'a> {
    pub fn new(actor: &'a mut InstanceActor, plugin_id: impl Into<String>) -> Self {
        Self {
            actor,
            plugin_id: plugin_id.into(),
        }
    }

    /// Send a prepared or partially prepared envelope through the one client
    /// path. The client supplies the active protocol identity and fence.
    pub async fn send(
        &mut self,
        envelope: Envelope,
        timeout: Duration,
    ) -> Result<Envelope, WorkerControlError> {
        let envelope = self.prepare_envelope(envelope)?;
        WorkerControlPort::send(self, envelope, timeout).await
    }

    pub async fn hello(
        &mut self,
        min_protocol_version: u32,
        max_protocol_version: u32,
        host_version: impl Into<String>,
        timeout: Duration,
    ) -> Result<HelloAck, WorkerControlError> {
        let response = self
            .send(
                Envelope {
                    payload: Some(Payload::Hello(Hello {
                        min_protocol_version,
                        max_protocol_version,
                        host_version: host_version.into(),
                    })),
                    ..Default::default()
                },
                timeout,
            )
            .await?;
        match response.payload {
            Some(Payload::HelloAck(ack))
                if ack.selected_protocol_version == CURRENT_PROTOCOL_VERSION
                    && (ack.plugin_id.is_empty() || ack.plugin_id == self.plugin_id) =>
            {
                Ok(ack)
            }
            Some(Payload::HelloAck(ack)) => Err(WorkerControlError::Protocol(format!(
                "invalid HelloAck selected version/plugin identity: version={}, plugin_id={}",
                ack.selected_protocol_version, ack.plugin_id
            ))),
            _ => Err(WorkerControlError::Protocol(
                "worker did not return HelloAck".to_string(),
            )),
        }
    }

    pub async fn configure(
        &mut self,
        settings: HashMap<String, String>,
        timeout: Duration,
    ) -> Result<(), WorkerControlError> {
        let response = self
            .send(
                Envelope {
                    payload: Some(Payload::Configure(Configure { settings })),
                    ..Default::default()
                },
                timeout,
            )
            .await?;
        match response.payload {
            Some(Payload::HealthStatus(_)) => Ok(()),
            Some(Payload::Error(error)) => Err(WorkerControlError::Remote(error.message)),
            _ => Err(WorkerControlError::Protocol(
                "worker did not acknowledge Configure with HealthStatus".to_string(),
            )),
        }
    }

    pub async fn invoke(
        &mut self,
        extension_point: impl Into<String>,
        method: impl Into<String>,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<InvokeResult, WorkerControlError> {
        self.invoke_message(
            Invoke {
                extension_point: extension_point.into(),
                method: method.into(),
                payload,
                payload_type_url: String::new(),
                request: None,
            },
            timeout,
        )
        .await
    }

    pub async fn invoke_message(
        &mut self,
        invoke: Invoke,
        timeout: Duration,
    ) -> Result<InvokeResult, WorkerControlError> {
        let response = self
            .send(
                Envelope {
                    payload: Some(Payload::Invoke(invoke)),
                    ..Default::default()
                },
                timeout,
            )
            .await?;
        match response.payload {
            Some(Payload::InvokeResult(result)) => Ok(result),
            Some(Payload::Error(error)) => Err(WorkerControlError::Remote(error.message)),
            _ => Err(WorkerControlError::Protocol(
                "worker did not return InvokeResult".to_string(),
            )),
        }
    }

    pub async fn cancel(
        &mut self,
        target_request_id: impl Into<String>,
        reason: impl Into<String>,
        timeout: Duration,
    ) -> Result<CancelAck, WorkerControlError> {
        let target_request_id = target_request_id.into();
        let response = self
            .send(
                Envelope {
                    payload: Some(Payload::Cancel(Cancel {
                        target_request_id: target_request_id.clone(),
                        reason: reason.into(),
                    })),
                    ..Default::default()
                },
                timeout,
            )
            .await?;
        match response.payload {
            Some(Payload::CancelAck(ack)) if ack.target_request_id == target_request_id => Ok(ack),
            Some(Payload::CancelAck(ack)) => Err(WorkerControlError::Protocol(format!(
                "CancelAck target mismatch: expected {}, got {}",
                target_request_id, ack.target_request_id
            ))),
            Some(Payload::Error(error)) => Err(WorkerControlError::Remote(error.message)),
            _ => Err(WorkerControlError::Protocol(
                "worker did not return CancelAck".to_string(),
            )),
        }
    }

    fn prepare_envelope(&self, mut envelope: Envelope) -> Result<Envelope, WorkerControlError> {
        if envelope.payload.is_none() {
            return Err(WorkerControlError::Protocol(
                "worker-control envelope must contain a payload".to_string(),
            ));
        }
        if envelope.request_id.is_empty() {
            envelope.request_id = uuid::Uuid::new_v4().to_string();
        }
        envelope.plugin_id = self.plugin_id.clone();
        envelope.protocol_version = CURRENT_PROTOCOL_VERSION;
        envelope.sequence_number = 0;
        envelope.generation = self.actor.generation();
        envelope.fence_token = self.actor.fence_token();
        Ok(envelope)
    }
}

#[async_trait]
impl WorkerControlPort for WorkerControlClient<'_> {
    async fn send(
        &mut self,
        envelope: Envelope,
        timeout: Duration,
    ) -> Result<Envelope, WorkerControlError> {
        let request_id = envelope.request_id.clone();
        let payload = envelope.encode_to_vec();
        let response = self
            .actor
            .send_request(request_id.clone(), payload, timeout)
            .await
            .map_err(|error| {
                if error.reason_code == "INVOKE_TIMEOUT" {
                    WorkerControlError::Timeout
                } else {
                    WorkerControlError::Transport(error)
                }
            })?;
        let response = Envelope::decode(response.as_slice())
            .map_err(|error| WorkerControlError::Protocol(error.to_string()))?;
        if response.request_id != request_id {
            return Err(WorkerControlError::Protocol(format!(
                "worker response request id mismatch: expected {}, got {}",
                request_id, response.request_id
            )));
        }
        if response.protocol_version != CURRENT_PROTOCOL_VERSION
            || response.plugin_id != self.plugin_id
        {
            return Err(WorkerControlError::Protocol(
                "worker response protocol or plugin identity mismatch".to_string(),
            ));
        }
        let expected_generation = self.actor.generation();
        let expected_fence = self.actor.fence_token();
        if response.generation != expected_generation || response.fence_token != expected_fence {
            return Err(WorkerControlError::FenceMismatch {
                expected_generation,
                expected_fence,
                actual_generation: response.generation,
                actual_fence: response.fence_token,
            });
        }
        Ok(response)
    }
}
