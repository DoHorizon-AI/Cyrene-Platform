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
        self, Cancel, CancelAck, Configure, Envelope, HealthCheck, HealthStatus, Hello, HelloAck,
        Invoke, InvokeResult, PluginErrorPayload, Shutdown,
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

    /// Hook invoked when the Kernel completes a fence rotation to a new lease generation.
    /// Workers should reset any in-flight cancellation tokens, clear generation-scoped caches,
    /// or re-synchronize internal session state.
    fn on_fence_rotated(&mut self, _new_generation: u64, _new_fence_token: u64) {}
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
    let mut active_generation: u64 = 0;
    let mut active_fence_token: u64 = 0;
    let mut has_request_sequence = false;
    let mut last_request_sequence = 0_u64;
    let mut handshake_complete = false;

    while let Some(req_env) = read_frame(&mut reader, max_frame_bytes)? {
        let req_id = req_env.request_id.clone();
        let trace_id = req_env.trace_id.clone();
        let generation = req_env.generation;
        let fence_token = req_env.fence_token;
        let plugin_id = worker.plugin_id().to_string();

        let is_stale = generation < active_generation
            || (generation == active_generation && fence_token < active_fence_token);

        let protocol_invalid = req_env.protocol_version != CURRENT_PROTOCOL_VERSION;
        let sequence_invalid =
            has_request_sequence && req_env.sequence_number <= last_request_sequence;
        let hello_required =
            !handshake_complete && !matches!(req_env.payload.as_ref(), Some(Payload::Hello(_)));

        let resp_payload = if protocol_invalid {
            Payload::Error(PluginErrorPayload {
                code: plugin_error_payload::Code::ProtocolError as i32,
                message: format!(
                    "unsupported worker protocol version {}; expected {}",
                    req_env.protocol_version, CURRENT_PROTOCOL_VERSION
                ),
                details: "PROTOCOL_VERSION_MISMATCH".to_string(),
            })
        } else if sequence_invalid {
            Payload::Error(PluginErrorPayload {
                code: plugin_error_payload::Code::ProtocolError as i32,
                message: format!(
                    "request sequence {} is not greater than previous sequence {}",
                    req_env.sequence_number, last_request_sequence
                ),
                details: "SEQUENCE_OUT_OF_ORDER".to_string(),
            })
        } else if hello_required {
            Payload::Error(PluginErrorPayload {
                code: plugin_error_payload::Code::ProtocolError as i32,
                message: "worker Hello handshake is required before control requests".to_string(),
                details: "HELLO_REQUIRED".to_string(),
            })
        } else if is_stale {
            Payload::Error(PluginErrorPayload {
                code: plugin_error_payload::Code::PermissionDenied as i32,
                message: format!(
                    "FENCED_OUT: request gen={}/fence={} is older than active gen={}/fence={}",
                    generation, fence_token, active_generation, active_fence_token
                ),
                details: "STALE_GENERATION".to_string(),
            })
        } else {
            has_request_sequence = true;
            last_request_sequence = req_env.sequence_number;
            if active_generation != 0
                && (generation > active_generation || fence_token > active_fence_token)
            {
                worker.on_fence_rotated(generation, fence_token);
            }
            active_generation = generation;
            active_fence_token = fence_token;

            match req_env.payload {
                Some(Payload::Hello(hello)) => {
                    if hello.min_protocol_version > hello.max_protocol_version
                        || hello.min_protocol_version > CURRENT_PROTOCOL_VERSION
                        || hello.max_protocol_version < CURRENT_PROTOCOL_VERSION
                    {
                        Payload::Error(PluginErrorPayload {
                            code: plugin_error_payload::Code::ProtocolError as i32,
                            message: "worker does not support the host protocol version"
                                .to_string(),
                            details: "INCOMPATIBLE_PROTOCOL_VERSION".to_string(),
                        })
                    } else {
                        handshake_complete = true;
                        Payload::HelloAck(HelloAck {
                            selected_protocol_version: CURRENT_PROTOCOL_VERSION,
                            plugin_id: worker.plugin_id().to_string(),
                            plugin_version: worker.plugin_version().to_string(),
                            api_version: worker.api_version().to_string(),
                            declared_capabilities: worker.declared_capabilities(),
                            metrics: worker.metrics(),
                            capabilities_json: worker.capabilities_json(),
                        })
                    }
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
                    Payload::CancelAck(CancelAck {
                        target_request_id: cancel.target_request_id,
                    })
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
                        generation,
                        fence_token,
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
            }
        };

        sequence_number = sequence_number.wrapping_add(1);
        let resp_env = Envelope {
            request_id: req_id,
            trace_id,
            plugin_id,
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number,
            generation,
            fence_token,
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
    use std::{
        collections::HashMap,
        io::Cursor,
        sync::{Arc, Mutex},
    };

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
            generation: 1,
            fence_token: 1,
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
            generation: 1,
            fence_token: 1,
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
            generation: 1,
            fence_token: 1,
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

    #[test]
    fn test_worker_generic_configure_invoke_cancel_lifecycle() {
        struct GenericWorker {
            configured: Arc<Mutex<HashMap<String, String>>>,
            cancelled: Arc<Mutex<Option<String>>>,
        }

        impl CyreneWorker for GenericWorker {
            fn plugin_id(&self) -> &str {
                "generic.worker"
            }
            fn plugin_version(&self) -> &str {
                "1.0.0"
            }
            fn api_version(&self) -> &str {
                "1"
            }
            fn on_configure(&mut self, settings: &HashMap<String, String>) -> Result<(), String> {
                *self.configured.lock().unwrap() = settings.clone();
                Ok(())
            }
            fn on_invoke(&mut self, invoke: Invoke) -> Result<InvokeResult, PluginErrorPayload> {
                assert_eq!(invoke.extension_point, "custom.capability");
                assert_eq!(invoke.method, "run");
                assert_eq!(invoke.payload, b"opaque-request");
                Ok(InvokeResult {
                    payload: b"opaque-result".to_vec(),
                    response: None,
                })
            }
            fn on_cancel(&mut self, target_request_id: &str, _reason: &str) {
                *self.cancelled.lock().unwrap() = Some(target_request_id.to_string());
            }
        }

        let configured = Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(Mutex::new(None));
        let worker = GenericWorker {
            configured: configured.clone(),
            cancelled: cancelled.clone(),
        };
        let mut input = Vec::new();
        let envelope = |request_id: &str, sequence_number: u64, payload: Payload| Envelope {
            request_id: request_id.to_string(),
            trace_id: "trace".to_string(),
            plugin_id: "generic.worker".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 1000,
            sequence_number,
            generation: 1,
            fence_token: 7,
            payload: Some(payload),
        };
        let hello = envelope(
            "hello",
            1,
            Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "host".to_string(),
            }),
        );
        write_frame(&mut input, &hello, DEFAULT_MAX_MESSAGE_BYTES).unwrap();
        let mut settings = HashMap::new();
        settings.insert("opaque_ref".to_string(), "ref-1".to_string());
        write_frame(
            &mut input,
            &envelope(
                "configure",
                2,
                Payload::Configure(Configure {
                    settings: settings.clone(),
                }),
            ),
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();
        write_frame(
            &mut input,
            &envelope(
                "invoke",
                3,
                Payload::Invoke(Invoke {
                    extension_point: "custom.capability".to_string(),
                    method: "run".to_string(),
                    payload: b"opaque-request".to_vec(),
                    request: None,
                }),
            ),
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();
        write_frame(
            &mut input,
            &envelope(
                "cancel",
                4,
                Payload::Cancel(Cancel {
                    target_request_id: "invoke".to_string(),
                    reason: "cooperative stop".to_string(),
                }),
            ),
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();
        write_frame(
            &mut input,
            &envelope(
                "shutdown",
                5,
                Payload::Shutdown(Shutdown {
                    grace_period_ms: 100,
                }),
            ),
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();

        let mut output = Vec::new();
        run_worker_stream(
            Cursor::new(input),
            &mut output,
            worker,
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();
        assert_eq!(*configured.lock().unwrap(), settings);
        assert_eq!(*cancelled.lock().unwrap(), Some("invoke".to_string()));

        let mut output_reader = Cursor::new(output);
        assert!(matches!(
            read_frame(&mut output_reader, DEFAULT_MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap()
                .payload,
            Some(Payload::HelloAck(_))
        ));
        assert!(matches!(
            read_frame(&mut output_reader, DEFAULT_MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap()
                .payload,
            Some(Payload::HealthStatus(_))
        ));
        let invoke_response = read_frame(&mut output_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        match invoke_response.payload {
            Some(Payload::InvokeResult(result)) => {
                assert_eq!(result.payload, b"opaque-result");
            }
            other => panic!("expected generic InvokeResult, got {other:?}"),
        }
        assert!(matches!(
            read_frame(&mut output_reader, DEFAULT_MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap()
                .payload,
            Some(Payload::CancelAck(_))
        ));
        assert!(matches!(
            read_frame(&mut output_reader, DEFAULT_MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap()
                .payload,
            Some(Payload::HealthStatus(_))
        ));
    }

    #[test]
    fn test_worker_fence_rotation_and_rejection() {
        use std::sync::{
            atomic::{AtomicU32, AtomicU64, Ordering},
            Arc,
        };

        struct TrackingWorker {
            rotated_calls: Arc<AtomicU32>,
            last_gen: Arc<AtomicU64>,
            last_fence: Arc<AtomicU64>,
        }

        impl CyreneWorker for TrackingWorker {
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

            fn on_fence_rotated(&mut self, new_generation: u64, new_fence_token: u64) {
                self.rotated_calls.fetch_add(1, Ordering::SeqCst);
                self.last_gen.store(new_generation, Ordering::SeqCst);
                self.last_fence.store(new_fence_token, Ordering::SeqCst);
            }
        }

        let rotated_calls = Arc::new(AtomicU32::new(0));
        let last_gen = Arc::new(AtomicU64::new(0));
        let last_fence = Arc::new(AtomicU64::new(0));

        let worker = TrackingWorker {
            rotated_calls: rotated_calls.clone(),
            last_gen: last_gen.clone(),
            last_fence: last_fence.clone(),
        };

        let mut input_buffer = Vec::new();

        // 1. Complete the required handshake at generation 2, fence 5.
        let hello_env = Envelope {
            request_id: "req-hello".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 0,
            generation: 2,
            fence_token: 5,
            payload: Some(Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        };
        write_frame(&mut input_buffer, &hello_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 2. Send HealthCheck with generation 2, fence 5
        let gen2_env = Envelope {
            request_id: "req-gen2".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 1,
            generation: 2,
            fence_token: 5,
            payload: Some(Payload::HealthCheck(HealthCheck {})),
        };
        write_frame(&mut input_buffer, &gen2_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 3. Advance generation to 3, fence 6 -> triggers on_fence_rotated
        let gen3_env = Envelope {
            request_id: "req-gen3".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 2,
            generation: 3,
            fence_token: 6,
            payload: Some(Payload::HealthCheck(HealthCheck {})),
        };
        write_frame(&mut input_buffer, &gen3_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 4. Send stale request with generation 1 (older than active generation 3)
        let stale_env = Envelope {
            request_id: "req-stale".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 3,
            generation: 1,
            fence_token: 5,
            payload: Some(Payload::HealthCheck(HealthCheck {})),
        };
        write_frame(&mut input_buffer, &stale_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        // 5. Shutdown
        let shutdown_env = Envelope {
            request_id: "req-shut".to_string(),
            trace_id: "tr-1".to_string(),
            plugin_id: "test.worker".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 4,
            generation: 3,
            fence_token: 6,
            payload: Some(Payload::Shutdown(Shutdown { grace_period_ms: 0 })),
        };
        write_frame(&mut input_buffer, &shutdown_env, DEFAULT_MAX_MESSAGE_BYTES).unwrap();

        let reader = Cursor::new(input_buffer);
        let mut output_buffer = Vec::new();

        run_worker_stream(
            reader,
            &mut output_buffer,
            worker,
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .unwrap();

        assert_eq!(
            rotated_calls.load(Ordering::SeqCst),
            1,
            "on_fence_rotated must be called once upon advancing from gen 2 to 3"
        );
        assert_eq!(last_gen.load(Ordering::SeqCst), 3);
        assert_eq!(last_fence.load(Ordering::SeqCst), 6);

        let mut out_reader = Cursor::new(output_buffer);

        // Resp 1: HelloAck
        let hello_resp = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(hello_resp.request_id, "req-hello");
        assert!(matches!(hello_resp.payload, Some(Payload::HelloAck(_))));

        // Resp 2: Success for generation 2
        let resp1 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp1.request_id, "req-gen2");
        assert!(matches!(resp1.payload, Some(Payload::HealthStatus(_))));

        // Resp 2: Success for generation 3
        let resp2 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp2.request_id, "req-gen3");
        assert!(matches!(resp2.payload, Some(Payload::HealthStatus(_))));

        // Resp 3: Error for stale generation 1
        let resp3 = read_frame(&mut out_reader, DEFAULT_MAX_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(resp3.request_id, "req-stale");
        match resp3.payload {
            Some(Payload::Error(err)) => {
                assert_eq!(
                    err.code,
                    plugin_error_payload::Code::PermissionDenied as i32
                );
                assert!(err.message.contains("FENCED_OUT"));
            }
            other => panic!("expected Error with FENCED_OUT, got {other:?}"),
        }
    }
}
