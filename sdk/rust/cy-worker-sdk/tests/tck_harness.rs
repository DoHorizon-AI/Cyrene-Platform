// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: sdk/rust/cy-worker-sdk/tests/tck_harness.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Automated TCK Conformance and Lifecycle Harness for CYRENE out-of-process Workers.

use std::{
    collections::HashMap,
    io::Cursor,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cy_worker_sdk::{
    health_status, read_frame, run_worker_stream, write_frame, CyreneWorker, Envelope, HealthCheck,
    HealthStatus, Hello, Payload, Shutdown, CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_MESSAGE_BYTES,
};

struct EchoWorker {
    plugin_id: String,
    health_calls: Arc<AtomicU32>,
    shutdown_calls: Arc<AtomicU32>,
}

impl EchoWorker {
    fn new(plugin_id: &str) -> (Self, Arc<AtomicU32>, Arc<AtomicU32>) {
        let health_calls = Arc::new(AtomicU32::new(0));
        let shutdown_calls = Arc::new(AtomicU32::new(0));
        (
            Self {
                plugin_id: plugin_id.to_string(),
                health_calls: health_calls.clone(),
                shutdown_calls: shutdown_calls.clone(),
            },
            health_calls,
            shutdown_calls,
        )
    }
}

impl CyreneWorker for EchoWorker {
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    fn plugin_version(&self) -> &str {
        "1.0.0"
    }

    fn api_version(&self) -> &str {
        "1.0"
    }

    fn declared_capabilities(&self) -> Vec<String> {
        vec!["ModelAnalyzer".to_string(), "Probe".to_string()]
    }

    fn metrics(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("backend".to_string(), "native-rust".to_string());
        m
    }

    fn on_health_check(&self) -> HealthStatus {
        self.health_calls.fetch_add(1, Ordering::SeqCst);
        HealthStatus {
            status: health_status::Status::Healthy as i32,
            message: "HEALTHY_OK".to_string(),
        }
    }

    fn on_shutdown(&mut self, _grace_period_ms: u32) {
        self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Simulated Host Watchdog Actor that drives Worker lifecycle over stream.
struct HostWatchdogSession {
    request_sequence: u64,
    crash_timestamps: Vec<Instant>,
    quarantined: bool,
}

impl HostWatchdogSession {
    fn new() -> Self {
        Self {
            request_sequence: 0,
            crash_timestamps: Vec::new(),
            quarantined: false,
        }
    }

    /// Record a crash and check if crash loop quarantine threshold (>3 crashes in 60s) is exceeded.
    fn record_crash(&mut self, now: Instant) -> bool {
        self.crash_timestamps.push(now);
        // Retain crashes within the last 60 seconds
        self.crash_timestamps
            .retain(|&t| now.duration_since(t) <= Duration::from_secs(60));
        if self.crash_timestamps.len() > 3 {
            self.quarantined = true;
        }
        self.quarantined
    }

    fn build_hello(&mut self) -> Envelope {
        self.request_sequence += 1;
        Envelope {
            request_id: format!("req-hello-{}", self.request_sequence),
            trace_id: "trace-tck".to_string(),
            plugin_id: "".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 5000,
            sequence_number: self.request_sequence,
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        }
    }

    fn build_health_check(&mut self, deadline_ms: i64) -> Envelope {
        self.request_sequence += 1;
        Envelope {
            request_id: format!("req-health-{}", self.request_sequence),
            trace_id: "trace-tck".to_string(),
            plugin_id: "".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms,
            sequence_number: self.request_sequence,
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::HealthCheck(HealthCheck {})),
        }
    }

    fn build_shutdown(&mut self, grace_period_ms: u32) -> Envelope {
        self.request_sequence += 1;
        Envelope {
            request_id: format!("req-shutdown-{}", self.request_sequence),
            trace_id: "trace-tck".to_string(),
            plugin_id: "".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: grace_period_ms as i64 + 1000,
            sequence_number: self.request_sequence,
            generation: 1,
            fence_token: 1,
            payload: Some(Payload::Shutdown(Shutdown { grace_period_ms })),
        }
    }
}

#[test]
fn test_tck_conformance_handshake_heartbeat_shutdown() {
    let mut watchdog = HostWatchdogSession::new();
    let (worker, health_counter, shutdown_counter) = EchoWorker::new("com.cyrene.test.worker");

    let mut host_tx = Vec::new();
    write_frame(
        &mut host_tx,
        &watchdog.build_hello(),
        DEFAULT_MAX_MESSAGE_BYTES,
    )
    .unwrap();
    write_frame(
        &mut host_tx,
        &watchdog.build_health_check(1000),
        DEFAULT_MAX_MESSAGE_BYTES,
    )
    .unwrap();
    write_frame(
        &mut host_tx,
        &watchdog.build_shutdown(500),
        DEFAULT_MAX_MESSAGE_BYTES,
    )
    .unwrap();

    let mut worker_tx = Vec::new();
    run_worker_stream(
        Cursor::new(host_tx),
        &mut worker_tx,
        worker,
        DEFAULT_MAX_MESSAGE_BYTES,
    )
    .unwrap();

    assert_eq!(health_counter.load(Ordering::SeqCst), 1);
    assert_eq!(shutdown_counter.load(Ordering::SeqCst), 1);

    let mut rx = Cursor::new(worker_tx);

    // 1. Verify HelloAck
    let ack_env = read_frame(&mut rx, DEFAULT_MAX_MESSAGE_BYTES)
        .unwrap()
        .unwrap();
    match ack_env.payload {
        Some(Payload::HelloAck(ack)) => {
            assert_eq!(ack.plugin_id, "com.cyrene.test.worker");
            assert_eq!(ack.selected_protocol_version, 1);
            assert!(ack
                .declared_capabilities
                .contains(&"ModelAnalyzer".to_string()));
            assert_eq!(ack.metrics.get("backend").unwrap(), "native-rust");
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    // 2. Verify HealthStatus
    let health_env = read_frame(&mut rx, DEFAULT_MAX_MESSAGE_BYTES)
        .unwrap()
        .unwrap();
    match health_env.payload {
        Some(Payload::HealthStatus(h)) => {
            assert_eq!(h.status, health_status::Status::Healthy as i32);
            assert_eq!(h.message, "HEALTHY_OK");
        }
        other => panic!("expected HealthStatus, got {other:?}"),
    }

    // 3. Verify Shutdown ACK
    let shut_env = read_frame(&mut rx, DEFAULT_MAX_MESSAGE_BYTES)
        .unwrap()
        .unwrap();
    match shut_env.payload {
        Some(Payload::HealthStatus(h)) => {
            assert_eq!(h.message, "Shutdown ACK");
        }
        other => panic!("expected Shutdown ACK, got {other:?}"),
    }
}

#[test]
fn test_watchdog_quarantine_crash_loop_isolation() {
    let mut watchdog = HostWatchdogSession::new();
    let base_time = Instant::now();

    // 1st crash at 0s
    assert!(!watchdog.record_crash(base_time));
    // 2nd crash at 10s
    assert!(!watchdog.record_crash(base_time + Duration::from_secs(10)));
    // 3rd crash at 20s
    assert!(!watchdog.record_crash(base_time + Duration::from_secs(20)));
    // 4th crash at 30s (>3 crashes within 60s) -> Quarantine!
    assert!(
        watchdog.record_crash(base_time + Duration::from_secs(30)),
        "watchdog must quarantine after >3 crashes in 60 seconds"
    );
    assert!(watchdog.quarantined);
}
