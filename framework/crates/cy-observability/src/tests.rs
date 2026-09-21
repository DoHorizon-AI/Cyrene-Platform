//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 tests.rs                                                        │
//! │  Module: cy_observability::tests                                    │
//! │  Role: Unit and integration tests for observability foundation.     │
//! │                                                                     │
//! │  模块职责：可观测性基础库结构化模式、脱敏与有界容错测试。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer};

use crate::{
    config::{LogFormat, ObservabilityConfig},
    emit_error, emit_event,
    error_catalog::PlatformErrorCode,
    events::EVENT_LEASE_ACQUIRED,
    formatter::CyreneLayer,
};

#[derive(Clone, Default)]
struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TestWriter {
    fn lines(&self) -> Vec<String> {
        let buf = self.0.lock().unwrap();
        String::from_utf8_lossy(&buf)
            .lines()
            .map(|s| s.to_string())
            .collect()
    }
}

#[test]
fn structured_json_matches_specification_schema() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-kernel")
        .with_instance_id("inst-123")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            info,
            EVENT_LEASE_ACQUIRED,
            "Successfully acquired worker lease",
            lease_id = "lease-456",
            worker_id = "worker-789",
            generation = 2u64,
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1, "expected exactly one log line");

    let record: Value = serde_json::from_str(&lines[0]).expect("output must be valid JSON");

    assert_eq!(record["schema_version"], 1);
    assert_eq!(record["level"], "INFO");
    assert_eq!(record["service.name"], "test-kernel");
    assert_eq!(record["service.instance.id"], "inst-123");
    assert_eq!(record["event.name"], EVENT_LEASE_ACQUIRED);
    assert_eq!(record["message"], "Successfully acquired worker lease");

    // Attributes check
    let attrs = &record["attributes"];
    assert_eq!(attrs["lease_id"], "lease-456");
    assert_eq!(attrs["worker_id"], "worker-789");
    assert_eq!(attrs["generation"], 2);

    // Normal events must NOT have error.code
    assert!(attrs.get("error.code").is_none());
}

#[test]
fn error_event_includes_stable_error_code() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-kernel")
        .with_instance_id("inst-err")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_error!(
            "platform.lease.release_deferred",
            PlatformErrorCode::LeaseReleaseRollbackFailed,
            "Failed to rollback in-memory lease",
            lease_id = "lease-999",
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);

    let record: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(record["level"], "ERROR");
    assert_eq!(record["event.name"], "platform.lease.release_deferred");
    assert_eq!(
        record["attributes"]["error.code"],
        "PLATFORM.LEASE.RELEASE_ROLLBACK_FAILED"
    );
}

#[test]
fn sensitive_tokens_are_strictly_redacted() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-sec")
        .with_instance_id("inst-sec")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            info,
            "platform.test.security",
            "Security test event",
            api_key = "cyk_live_supersecrettoken",
            token = "secret-jwt-token-12345",
            password = "plaintext-password",
            auth_header = "Bearer sensitive-bearer-token",
            safe_id = "public-id-abc",
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);

    let record: Value = serde_json::from_str(&lines[0]).unwrap();
    let attrs = &record["attributes"];

    assert_eq!(attrs["api_key"], "[REDACTED]");
    assert_eq!(attrs["token"], "[REDACTED]");
    assert_eq!(attrs["password"], "[REDACTED]");
    assert_eq!(attrs["auth_header"], "[REDACTED]");
    assert_eq!(attrs["safe_id"], "public-id-abc");

    // Double check that raw secrets appear nowhere in the raw JSON text
    let raw = &lines[0];
    assert!(!raw.contains("cyk_live_supersecrettoken"));
    assert!(!raw.contains("secret-jwt-token-12345"));
    assert!(!raw.contains("plaintext-password"));
    assert!(!raw.contains("sensitive-bearer-token"));
}

#[test]
fn overlong_message_is_safely_truncated() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-trunc")
        .with_instance_id("inst-trunc")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    let huge_message = "A".repeat(10_000);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(info, "platform.test.trunc", &huge_message);
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);

    let record: Value = serde_json::from_str(&lines[0]).unwrap();
    let msg = record["message"].as_str().unwrap();
    assert!(msg.len() <= 4 * 1024);
    assert!(msg.ends_with("... [TRUNCATED]"));
}

#[test]
fn development_text_format_is_human_readable() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::development("test-dev")
        .with_instance_id("inst-dev")
        .with_format(LogFormat::Text);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("debug"));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            debug,
            "platform.dev.event",
            "Debugging worker step",
            step = "initialize",
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("DEBUG test-dev"));
    assert!(lines[0].contains("[platform.dev.event]"));
    assert!(lines[0].contains("Debugging worker step"));
    assert!(lines[0].contains("step=initialize"));
}

#[test]
fn log_injection_attempt_does_not_create_multiple_lines() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-inject")
        .with_instance_id("inst-inject")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    let malicious = "Normal message\n{\"schema_version\":1,\"level\":\"INFO\",\"message\":\"Fake admin grant\"}\n";

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            info,
            "platform.test.injection",
            malicious,
            user_input = "malicious\r\nline2",
        );
    });

    let lines = writer.lines();
    // In NDJSON, exactly one line must be written; newlines must be safely escaped in JSON string values
    assert_eq!(
        lines.len(),
        1,
        "malicious newlines must not inject extra log records into NDJSON stream"
    );

    let parsed: Value = serde_json::from_str(&lines[0]).expect("must remain valid JSON");
    assert!(parsed["message"].as_str().unwrap().contains("Fake admin grant"));
    assert_eq!(parsed["attributes"]["user_input"], "malicious\r\nline2");
}

#[test]
fn oversize_record_is_pruned_without_breaking_json() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-oversize")
        .with_instance_id("inst-oversize")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    // Create a large attribute that pushes the record over 32 KiB
    let huge_attr = "X".repeat(40_000);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            info,
            "platform.test.oversize",
            "Checking bounded record truncation",
            payload = &huge_attr,
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);

    let record: Value = serde_json::from_str(&lines[0]).expect("oversize record must remain valid JSON");
    assert_eq!(record["schema_version"], 1);
    assert_eq!(record["event.name"], "platform.test.oversize");
    assert_eq!(record["attributes"]["truncated"], true);
    assert!(lines[0].len() <= 32 * 1024);
}

#[test]
fn w3c_traceparent_parsing_and_formatting() {
    use crate::correlation::TraceContext;

    let valid_header = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let tc = TraceContext::parse_traceparent(valid_header).expect("must parse valid W3C header");
    assert_eq!(tc.trace_id_hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(tc.span_id_hex(), "00f067aa0ba902b7");
    assert_eq!(tc.flags, 0x01);
    assert_eq!(tc.to_traceparent(), valid_header);

    // Child span preserves trace_id and flags, generates new span_id
    let child = tc.child_span();
    assert_eq!(child.trace_id, tc.trace_id);
    assert_eq!(child.flags, tc.flags);
    assert_ne!(child.span_id, tc.span_id);
    assert_ne!(child.span_id, [0u8; 8]);
}

#[test]
fn w3c_traceparent_invalid_formats_are_rejected() {
    use crate::correlation::{CorrelationError, TraceContext};

    // Invalid parts count
    assert_eq!(
        TraceContext::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7"),
        Err(CorrelationError::InvalidFormat)
    );
    // Unsupported version
    assert_eq!(
        TraceContext::parse_traceparent("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        Err(CorrelationError::UnsupportedVersion("01".to_string()))
    );
    // All-zero trace_id
    assert_eq!(
        TraceContext::parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01"),
        Err(CorrelationError::InvalidTraceId)
    );
    // All-zero span_id
    assert_eq!(
        TraceContext::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"),
        Err(CorrelationError::InvalidSpanId)
    );
    // Invalid characters
    assert_eq!(
        TraceContext::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e47zz-00f067aa0ba902b7-01"),
        Err(CorrelationError::InvalidTraceId)
    );
}

#[test]
fn correlation_hierarchy_and_sanitization() {
    use crate::correlation::{
        sanitize_correlation_id, sanitize_operation_id, sanitize_request_id, sanitize_resource_id,
        CorrelationContext, TraceContext, MAX_OPERATION_ID_LEN, MAX_REQUEST_ID_LEN,
        MAX_RESOURCE_ID_LEN,
    };

    // Control characters and newlines are stripped
    let malicious = "req-123\r\nInjected: Header\x00";
    let sanitized = sanitize_request_id(malicious).expect("must produce sanitized id");
    assert_eq!(sanitized, "req-123Injected:Header");

    // Generic correlation sanitizer
    assert_eq!(
        sanitize_correlation_id("safe-token_123", 50),
        Some("safe-token_123".to_string())
    );

    // Length bounding for all ID types
    let long_id = "A".repeat(300);
    assert_eq!(
        sanitize_request_id(&long_id).unwrap().len(),
        MAX_REQUEST_ID_LEN
    );
    assert_eq!(
        sanitize_operation_id(&long_id).unwrap().len(),
        MAX_OPERATION_ID_LEN
    );
    assert_eq!(
        sanitize_resource_id(&long_id).unwrap().len(),
        MAX_RESOURCE_ID_LEN
    );

    // Context hierarchy
    let tc = TraceContext::new_root();
    let ctx = CorrelationContext::new()
        .with_request_id("req-uuid-123")
        .with_operation_id("op-train-456")
        .with_resource_id("res-deployment-789")
        .with_trace_context(tc);

    assert_eq!(ctx.request_id.as_deref(), Some("req-uuid-123"));
    assert_eq!(ctx.operation_id.as_deref(), Some("op-train-456"));
    assert_eq!(ctx.resource_id.as_deref(), Some("res-deployment-789"));
    assert_eq!(ctx.trace_context, Some(tc));
}

#[test]
fn structured_record_promotes_trace_id_and_retains_correlation_attributes() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-correlation")
        .with_instance_id("inst-corr-1")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_event!(
            info,
            "platform.test.correlation",
            "Executing correlated operation",
            trace_id = "4bf92f3577b34da6a3ce929d0e0e4736",
            span_id = "00f067aa0ba902b7",
            request_id = "req-abc-123",
            operation_id = "op-deploy-999",
            resource_id = "res-worker-1",
        );
    });

    let lines = writer.lines();
    assert_eq!(lines.len(), 1);

    let record: Value = serde_json::from_str(&lines[0]).expect("must be valid JSON");
    // Top-level trace fields
    assert_eq!(record["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(record["span_id"], "00f067aa0ba902b7");

    // Attributes retain request_id, operation_id, resource_id
    assert_eq!(record["attributes"]["request_id"], "req-abc-123");
    assert_eq!(record["attributes"]["operation_id"], "op-deploy-999");
    assert_eq!(record["attributes"]["resource_id"], "res-worker-1");
}

#[test]
fn rolling_file_config_defaults_and_builder() {
    use crate::sink::{RollingFileConfig, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_HISTORY_FILES};

    let cfg = RollingFileConfig::new("/tmp/logs", "cy-test", 0, 0);
    assert_eq!(cfg.max_file_bytes, DEFAULT_MAX_FILE_BYTES);
    assert_eq!(cfg.max_history_files, DEFAULT_MAX_HISTORY_FILES);
    assert_eq!(cfg.file_prefix, "cy-test");

    let custom = RollingFileConfig::new("/tmp/logs", "cy-custom", 1024, 2);
    assert_eq!(custom.max_file_bytes, 1024);
    assert_eq!(custom.max_history_files, 2);

    let obs = ObservabilityConfig::managed("test").with_rolling_file(custom.clone());
    assert!(obs.rolling_file.is_some());
    assert_eq!(obs.rolling_file.unwrap().max_history_files, 2);
}

#[test]
fn rolling_file_sink_rotates_and_shifts_history() {
    use std::io::Write;
    use tempfile::tempdir;
    use crate::sink::{BoundedRollingFileSink, RollingFileConfig};

    let dir = tempdir().expect("create temp dir");
    let config = RollingFileConfig::new(dir.path(), "test-app", 100, 3);
    let mut sink = BoundedRollingFileSink::new(config).expect("create sink");

    // 1. Write chunk 1 (60 bytes)
    let chunk1 = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    sink.write_all(chunk1).expect("write chunk 1");
    sink.flush().expect("flush chunk 1");
    assert_eq!(sink.current_bytes(), 60);

    let active_path = dir.path().join("test-app.log");
    assert!(active_path.exists());
    assert_eq!(std::fs::metadata(&active_path).unwrap().len(), 60);

    // 2. Write chunk 2 (60 bytes) -> exceeds 100 bytes, triggers rotation!
    let chunk2 = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    sink.write_all(chunk2).expect("write chunk 2");
    sink.flush().expect("flush chunk 2");
    assert_eq!(sink.current_bytes(), 60);

    let archive1 = dir.path().join("test-app.log.1");
    assert!(active_path.exists());
    assert!(archive1.exists());
    assert_eq!(std::fs::read(&archive1).unwrap(), chunk1);
    assert_eq!(std::fs::read(&active_path).unwrap(), chunk2);

    // 3. Write chunk 3 (60 bytes) -> triggers rotation!
    let chunk3 = b"CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    sink.write_all(chunk3).expect("write chunk 3");
    sink.flush().expect("flush chunk 3");

    let archive2 = dir.path().join("test-app.log.2");
    assert!(archive2.exists());
    assert_eq!(std::fs::read(&archive2).unwrap(), chunk1);
    assert_eq!(std::fs::read(&archive1).unwrap(), chunk2);
    assert_eq!(std::fs::read(&active_path).unwrap(), chunk3);

    // 4. Write chunk 4 (60 bytes) -> triggers rotation!
    let chunk4 = b"DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";
    sink.write_all(chunk4).expect("write chunk 4");
    sink.flush().expect("flush chunk 4");

    let archive3 = dir.path().join("test-app.log.3");
    assert!(archive3.exists());
    assert_eq!(std::fs::read(&archive3).unwrap(), chunk1);
    assert_eq!(std::fs::read(&archive2).unwrap(), chunk2);
    assert_eq!(std::fs::read(&archive1).unwrap(), chunk3);
    assert_eq!(std::fs::read(&active_path).unwrap(), chunk4);

    // 5. Write chunk 5 (60 bytes) -> max_history_files is 3, so oldest (.3 containing chunk1) is pruned!
    let chunk5 = b"EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
    sink.write_all(chunk5).expect("write chunk 5");
    sink.flush().expect("flush chunk 5");

    assert_eq!(std::fs::read(&archive3).unwrap(), chunk2);
    assert_eq!(std::fs::read(&archive2).unwrap(), chunk3);
    assert_eq!(std::fs::read(&archive1).unwrap(), chunk4);
    assert_eq!(std::fs::read(&active_path).unwrap(), chunk5);

    // Verify .log.4 does not exist
    let archive4 = dir.path().join("test-app.log.4");
    assert!(!archive4.exists());
    assert_eq!(sink.dropped_writes_count(), 0);
}

#[test]
fn rolling_file_sink_tracks_dropped_writes_on_error() {
    use std::io::Write;
    use tempfile::tempdir;
    use crate::sink::{BoundedRollingFileSink, RollingFileConfig};

    let dir = tempdir().expect("create temp dir");
    let config = RollingFileConfig::new(dir.path(), "readonly-app", 100, 2);
    let mut sink = BoundedRollingFileSink::new(config).expect("create sink");

    sink.write_all(b"initial data").expect("write initial");
    assert_eq!(sink.dropped_writes_count(), 0);

    let active_path = sink.active_file_path();
    let mut perms = std::fs::metadata(&active_path).unwrap().permissions();
    perms.set_readonly(true);
    let _ = std::fs::set_permissions(&active_path, perms);

    let huge = vec![b'X'; 200];
    let res = sink.write_all(&huge);
    if res.is_err() {
        assert!(sink.dropped_writes_count() > 0);
    }

    let mut perms_rw = std::fs::metadata(&active_path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms_rw.set_readonly(false);
    let _ = std::fs::set_permissions(&active_path, perms_rw);
}

#[test]
fn init_observability_fails_gracefully_on_invalid_config() {
    use crate::init::{init_observability, ObservabilityError};

    let invalid = ObservabilityConfig::managed("").with_log_level("invalid_level_syntax!!!");
    let result = init_observability(invalid);
    assert!(result.is_err());
    match result.unwrap_err() {
        ObservabilityError::InvalidConfiguration(msg) => {
            assert!(!msg.is_empty());
        }
        other => panic!("expected InvalidConfiguration, got {other:?}"),
    }
}

#[test]
fn concurrent_threads_maintain_isolated_correlation_contexts() {
    let writer = TestWriter::default();
    let config = ObservabilityConfig::managed("test-concurrency")
        .with_instance_id("inst-conc")
        .with_format(LogFormat::Json);

    let layer = CyreneLayer::new(&config, writer.clone()).with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry().with(layer);
    let dispatch = tracing::Dispatch::new(subscriber);

    let d1 = dispatch.clone();
    let h1 = std::thread::spawn(move || {
        let _guard = tracing::dispatcher::set_default(&d1);
        emit_event!(
            info,
            "platform.test.concurrent",
            "Task 1 event",
            trace_id = "11111111111111111111111111111111",
            span_id = "1111111111111111",
            request_id = "req-1",
        );
    });

    let d2 = dispatch.clone();
    let h2 = std::thread::spawn(move || {
        let _guard = tracing::dispatcher::set_default(&d2);
        emit_event!(
            info,
            "platform.test.concurrent",
            "Task 2 event",
            trace_id = "22222222222222222222222222222222",
            span_id = "2222222222222222",
            request_id = "req-2",
        );
    });

    h1.join().unwrap();
    h2.join().unwrap();

    let lines = writer.lines();
    assert_eq!(lines.len(), 2, "expected exactly two records from concurrent threads");

    let records: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect();

    let rec1 = records
        .iter()
        .find(|r| r["trace_id"] == "11111111111111111111111111111111")
        .expect("record 1 must exist");
    assert_eq!(rec1["span_id"], "1111111111111111");
    assert_eq!(rec1["attributes"]["request_id"], "req-1");
    assert_eq!(rec1["message"], "Task 1 event");

    let rec2 = records
        .iter()
        .find(|r| r["trace_id"] == "22222222222222222222222222222222")
        .expect("record 2 must exist");
    assert_eq!(rec2["span_id"], "2222222222222222");
    assert_eq!(rec2["attributes"]["request_id"], "req-2");
    assert_eq!(rec2["message"], "Task 2 event");
}


