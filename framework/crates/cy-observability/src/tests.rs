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

