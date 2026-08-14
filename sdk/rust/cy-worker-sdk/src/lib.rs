//! Lightweight SDK for building out-of-process CYRENE Native Workers.
//!
//! CYRENE workers run as standalone operating system child processes. Communication
//! is conducted strictly via 4-byte length-prefixed Protobuf envelopes over standard I/O (or UDS).
//! Standard output (`stdout`) is strictly reserved for protocol binary frames.
//! Diagnostic logs MUST be written to standard error (`stderr`).

use std::{
    collections::HashMap,
    io::{self, Read, Write},
};

pub use cy_plugin_protocol::{
    envelope::Payload,
    health_status,
    pb::{
        self, Cancel, Configure, Envelope, HealthCheck, HealthStatus, Hello, HelloAck, Invoke,
        InvokeResult, PluginErrorPayload, Shutdown,
    },
    plugin_error_payload, CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_MESSAGE_BYTES,
};
use prost::Message;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("Protobuf encode error: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("Frame too large ({0} bytes > {1} bytes limit)")]
    FrameTooLarge(usize, usize),
    #[error("Protocol error: {0}")]
    Protocol(String),
}

/// Trait defining the lifecycle and execution hooks for an out-of-process Worker.
pub trait CyreneWorker: Send + Sync {
    /// Unique plugin/worker identifier (e.g. `"com.cyrene.plugin.model-analyzer"`).
    fn plugin_id(&self) -> &str;

    /// Semantic version of the worker (e.g. `"1.0.0"`).
    fn plugin_version(&self) -> &str;

    /// API version supported by the worker (e.g. `"1.0"`).
    fn api_version(&self) -> &str;

    /// Declared capabilities (e.g. `["ModelAnalyzer", "Probe"]`).
    fn declared_capabilities(&self) -> Vec<String> {
        Vec::new()
    }

    /// Dynamic metrics or metadata to report during handshake.
    fn metrics(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    /// JSON representation of extension point capabilities.
    fn capabilities_json(&self) -> String {
        "{}".to_string()
    }

    /// Hook invoked when the host sends configuration settings.
    fn on_configure(&mut self, _settings: &HashMap<String, String>) -> Result<(), String> {
        Ok(())
    }

    /// Hook invoked on health check probes.
    fn on_health_check(&self) -> HealthStatus {
        HealthStatus {
            status: health_status::Status::Healthy as i32,
            message: "OK".to_string(),
        }
    }

    /// Hook invoked when an invoke RPC request arrives.
    fn on_invoke(&mut self, _invoke: Invoke) -> Result<InvokeResult, PluginErrorPayload> {
        Err(PluginErrorPayload {
            code: plugin_error_payload::Code::ExecutionFailed as i32,
            message: "on_invoke not implemented by this worker".to_string(),
            details: String::new(),
        })
    }

    /// Hook invoked when a cancellation arrives for an active request.
    fn on_cancel(&mut self, _target_request_id: &str, _reason: &str) {}

    /// Hook invoked when the host requests graceful shutdown.
    fn on_shutdown(&mut self, _grace_period_ms: u32) {}
}

/// Read one length-prefixed Envelope from reader.
pub fn read_frame<R: Read>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<Envelope>, WorkerError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(WorkerError::Io(e)),
    }

    let payload_len = u32::from_be_bytes(len_buf) as usize;
    if payload_len > max_bytes {
        return Err(WorkerError::FrameTooLarge(payload_len, max_bytes));
    }

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload)?;
    let envelope = Envelope::decode(&payload[..])?;
    Ok(Some(envelope))
}

/// Write one length-prefixed Envelope to writer and flush.
pub fn write_frame<W: Write>(
    writer: &mut W,
    envelope: &Envelope,
    max_bytes: usize,
) -> Result<(), WorkerError> {
    let payload_len = envelope.encoded_len();
    if payload_len > max_bytes {
        return Err(WorkerError::FrameTooLarge(payload_len, max_bytes));
    }

    let len_bytes = (payload_len as u32).to_be_bytes();
    writer.write_all(&len_bytes)?;
    let mut buf = Vec::with_capacity(payload_len);
    envelope.encode(&mut buf)?;
    writer.write_all(&buf)?;
    writer.flush()?;
    Ok(())
}

/// Run the worker protocol loop on custom streams (useful for testing or UDS).
pub fn run_worker_stream<R: Read, W: Write, T: CyreneWorker>(
    mut reader: R,
    mut writer: W,
    mut worker: T,
    max_frame_bytes: usize,
) -> Result<(), WorkerError> {
    let mut sequence_number: u64 = 0;

    while let Some(req_env) = read_frame(&mut reader, max_frame_bytes)? {
        let req_id = req_env.request_id.clone();
        let trace_id = req_env.trace_id.clone();
        let plugin_id = worker.plugin_id().to_string();

        let resp_payload = match req_env.payload {
            Some(Payload::Hello(hello)) => {
                let selected_version = hello
                    .max_protocol_version
                    .min(CURRENT_PROTOCOL_VERSION)
                    .max(hello.min_protocol_version);

                Payload::HelloAck(HelloAck {
                    selected_protocol_version: selected_version,
                    plugin_id: worker.plugin_id().to_string(),
                    plugin_version: worker.plugin_version().to_string(),
                    api_version: worker.api_version().to_string(),
                    declared_capabilities: worker.declared_capabilities(),
                    metrics: worker.metrics(),
                    capabilities_json: worker.capabilities_json(),
                })
            }
            Some(Payload::HealthCheck(_)) => {
                let status = worker.on_health_check();
                Payload::HealthStatus(status)
            }
            Some(Payload::Configure(conf)) => match worker.on_configure(&conf.settings) {
                Ok(()) => Payload::HealthStatus(HealthStatus {
                    status: health_status::Status::Healthy as i32,
                    message: "Configured".to_string(),
                }),
                Err(err) => Payload::Error(PluginErrorPayload {
                    code: plugin_error_payload::Code::InvalidInput as i32,
                    message: err,
                    details: String::new(),
                }),
            },
            Some(Payload::Invoke(invoke)) => match worker.on_invoke(invoke) {
                Ok(result) => Payload::InvokeResult(result),
                Err(error) => Payload::Error(error),
            },
            Some(Payload::Cancel(cancel)) => {
                worker.on_cancel(&cancel.target_request_id, &cancel.reason);
                continue;
            }
            Some(Payload::Shutdown(shutdown)) => {
                worker.on_shutdown(shutdown.grace_period_ms);
                // Acknowledge shutdown with healthy status before terminating loop
                let resp_env = Envelope {
                    request_id: req_id,
                    trace_id,
                    plugin_id,
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    deadline_ms: 0,
                    sequence_number: sequence_number.wrapping_add(1),
                    payload: Some(Payload::HealthStatus(HealthStatus {
                        status: health_status::Status::Healthy as i32,
                        message: "Shutdown ACK".to_string(),
                    })),
                };
                write_frame(&mut writer, &resp_env, max_frame_bytes)?;
                break;
            }
            _ => Payload::Error(PluginErrorPayload {
                code: plugin_error_payload::Code::ProtocolError as i32,
                message: "Unsupported payload received by worker".to_string(),
                details: String::new(),
            }),
        };

        sequence_number = sequence_number.wrapping_add(1);
        let resp_env = Envelope {
            request_id: req_id,
            trace_id,
            plugin_id,
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number,
            payload: Some(resp_payload),
        };

        write_frame(&mut writer, &resp_env, max_frame_bytes)?;
    }

    Ok(())
}

/// Standard entry point for out-of-process Workers communicating over stdin / stdout.
pub fn run_worker_stdio<T: CyreneWorker>(worker: T) -> Result<(), WorkerError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_worker_stream(
        stdin.lock(),
        stdout.lock(),
        worker,
        DEFAULT_MAX_MESSAGE_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct TestWorker;
    impl CyreneWorker for TestWorker {
        fn plugin_id(&self) -> &str {
            "test.worker"
        }
        fn plugin_version(&self) -> &str {
            "1.0.0"
        }
        fn api_version(&self) -> &str {
            "1.0"
        }
        fn declared_capabilities(&self) -> Vec<String> {
            vec!["Probe".to_string()]
        }
    }

    #[test]
    fn test_worker_lifecycle_stream() {
        let mut input_buffer = Vec::new();

        // 1. Send Hello
        let hello_env = Envelope {
            request_id: "req-1".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 5000,
            sequence_number: 0,
            payload: Some(Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        };
        write_frame(&mut input_buffer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 2. Send HealthCheck
        let health_env = Envelope {
            request_id: "req-2".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 1,
            payload: Some(Payload::HealthCheck(HealthCheck {})),
        };
        write_frame(&mut input_buffer, &health_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 3. Send Shutdown
        let shutdown_env = Envelope {
            request_id: "req-3".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 2000,
            sequence_number: 2,
            payload: Some(Payload::Shutdown(Shutdown {
                grace_period_ms: 500,
            })),
        };
        write_frame(&mut input_buffer, &shutdown_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        let reader = Cursor::new(input_buffer);
        let mut output_buffer = Vec::new();

        run_worker_stream(
            reader,
            &mut output_buffer,
            TestWorker,
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();

        // Read outputs from worker
        let mut out_reader = Cursor::new(output_buffer);

        // Ack 1: HelloAck
        let resp1 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp1.request_id, "req-1");
        match resp1.payload {
            Some(Payload::HelloAck(ack)) => {
                assert_eq!(ack.plugin_id, "test.worker");
                assert_eq!(ack.selected_protocol_version, 1);
                assert_eq!(ack.declared_capabilities, vec!["Probe".to_string()]);
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }

        // Ack 2: HealthStatus
        let resp2 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp2.request_id, "req-2");
        match resp2.payload {
            Some(Payload::HealthStatus(h)) => {
                assert_eq!(h.status, health_status::Status::Healthy as i32);
            }
            other => panic!("expected HealthStatus, got {other:?}"),
        }

        // Ack 3: Shutdown ACK
        let resp3 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp3.request_id, "req-3");
        match resp3.payload {
            Some(Payload::HealthStatus(h)) => {
                assert_eq!(h.message, "Shutdown ACK");
            }
            other => panic!("expected Shutdown ACK, got {other:?}"),
        }

        // Stream EOF
        let resp4 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES).unwrap();
        assert!(resp4.is_none());
    }
}
