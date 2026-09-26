//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 formatter.rs                                                    │
//! │  Module: cy_observability::formatter                                │
//! │  Role: Custom Layer for strict Cyrene NDJSON & development text.    │
//! │                                                                     │
//! │  模块职责：严格符合 Cyrene 规范的结构化 NDJSON 与开发文本格式化器。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{
    fmt,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use chrono::Utc;
use serde_json::{json, Map, Value};
use tracing::{
    field::{Field, Visit},
    Event, Level, Subscriber,
};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::{
    config::{LogFormat, ObservabilityConfig},
    redaction::{
        is_sensitive_key, sanitize_field, truncate_bounded, DEFAULT_MAX_MESSAGE_BYTES,
        DEFAULT_MAX_RECORD_BYTES,
    },
};

/// ════════════════════════════════════════════════════════════════════════
/// Custom Tracing Layer enforcing Cyrene's structured record model.
///
/// 实现了 Cyrene 第一方结构化日志规范的 Tracing Layer。
/// ════════════════════════════════════════════════════════════════════════
pub struct CyreneLayer<W: Write + 'static> {
    service_name: String,
    service_instance_id: String,
    format: LogFormat,
    max_record_bytes: usize,
    max_message_bytes: usize,
    writer: Arc<Mutex<W>>,
    dropped_records: Arc<AtomicU64>,
}

impl<W: Write + 'static> CyreneLayer<W> {
    pub fn new(config: &ObservabilityConfig, writer: W) -> Self {
        Self {
            service_name: config.service_name.clone(),
            service_instance_id: config.service_instance_id.clone(),
            format: config.format,
            max_record_bytes: if config.max_record_bytes == 0 {
                DEFAULT_MAX_RECORD_BYTES
            } else {
                config.max_record_bytes
            },
            max_message_bytes: if config.max_message_bytes == 0 {
                DEFAULT_MAX_MESSAGE_BYTES
            } else {
                config.max_message_bytes
            },
            writer: Arc::new(Mutex::new(writer)),
            dropped_records: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns the number of records dropped due to serialization/sink errors.
    /// 中文：返回因序列化或 sink 错误而被丢弃的记录数量。
    pub fn dropped_records_count(&self) -> u64 {
        self.dropped_records.load(Ordering::Relaxed)
    }
}

struct FieldVisitor {
    message: Option<String>,
    event_name: Option<String>,
    attributes: Map<String, Value>,
}

impl FieldVisitor {
    fn new() -> Self {
        Self {
            message: None,
            event_name: None,
            attributes: Map::new(),
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let name = field.name();
        let formatted = format!("{value:?}");
        self.record_str_value(name, &formatted);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_str_value(field.name(), value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        let name = field.name();
        if is_sensitive_key(name) {
            self.attributes
                .insert(name.to_string(), Value::String("[REDACTED]".to_string()));
        } else {
            self.attributes
                .insert(name.to_string(), Value::Number(value.into()));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        let name = field.name();
        if is_sensitive_key(name) {
            self.attributes
                .insert(name.to_string(), Value::String("[REDACTED]".to_string()));
        } else {
            self.attributes
                .insert(name.to_string(), Value::Number(value.into()));
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        let name = field.name();
        if is_sensitive_key(name) {
            self.attributes
                .insert(name.to_string(), Value::String("[REDACTED]".to_string()));
        } else {
            self.attributes.insert(name.to_string(), Value::Bool(value));
        }
    }
}

impl FieldVisitor {
    fn record_str_value(&mut self, name: &str, raw_value: &str) {
        if name == "message" {
            self.message = Some(raw_value.to_string());
        } else if name == "event.name" || name == "event_name" {
            self.event_name = Some(raw_value.to_string());
        } else {
            let sanitized = sanitize_field(name, raw_value);
            self.attributes
                .insert(name.to_string(), Value::String(sanitized.into_owned()));
        }
    }
}

impl<S, W> Layer<S> for CyreneLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: Write + 'static,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let timestamp = Utc::now().to_rfc3339();
        let level = match *event.metadata().level() {
            Level::TRACE => "TRACE",
            Level::DEBUG => "DEBUG",
            Level::INFO => "INFO",
            Level::WARN => "WARN",
            Level::ERROR => "ERROR",
        };

        let mut visitor = FieldVisitor::new();
        event.record(&mut visitor);

        // Inherit span attributes if within a span
        // 中文：如果当前位于 span 内，则继承该 span 的属性。
        let mut span_id = None;
        let mut trace_id = None;
        if let Some(current_span) = ctx.lookup_current() {
            span_id = Some(format!("{:016x}", current_span.id().into_u64()));
        }
        if let Some(tid) = visitor.attributes.remove("trace_id").and_then(|v| match v {
            Value::String(s) => Some(s),
            _ => None,
        }) {
            trace_id = Some(tid);
        }
        if let Some(sid) = visitor.attributes.remove("span_id").and_then(|v| match v {
            Value::String(s) => Some(s),
            _ => None,
        }) {
            span_id = Some(sid);
        }

        let raw_message = visitor.message.unwrap_or_default();
        let (message, _) = truncate_bounded(&raw_message, self.max_message_bytes);

        match self.format {
            LogFormat::Json => {
                let mut record = json!({
                    "schema_version": 1,
                    "timestamp": timestamp,
                    "level": level,
                    "service.name": &self.service_name,
                    "service.instance.id": &self.service_instance_id,
                    "message": message,
                });

                if let Some(obj) = record.as_object_mut() {
                    if let Some(ev_name) = visitor.event_name {
                        obj.insert("event.name".to_string(), Value::String(ev_name));
                    }

                    if let Some(tid) = trace_id {
                        obj.insert("trace_id".to_string(), Value::String(tid));
                    }
                    if let Some(sid) = span_id {
                        obj.insert("span_id".to_string(), Value::String(sid));
                    }

                    if !visitor.attributes.is_empty() {
                        obj.insert("attributes".to_string(), Value::Object(visitor.attributes));
                    }
                }

                // Check serialized size budget
                // 中文：检查序列化后的大小预算。
                let mut serialized = match serde_json::to_string(&record) {
                    Ok(s) => s,
                    Err(_) => {
                        self.dropped_records.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                if serialized.len() > self.max_record_bytes {
                    // Prune attributes to ensure valid JSON within budget
                    // 中文：精简属性，确保 JSON 在大小预算内仍然有效。
                    if let Some(attrs) =
                        record.get_mut("attributes").and_then(|a| a.as_object_mut())
                    {
                        attrs.clear();
                        attrs.insert("truncated".to_string(), Value::Bool(true));
                    }
                    if let Ok(truncated_json) = serde_json::to_string(&record) {
                        serialized = truncated_json;
                    }
                }

                if let Ok(mut writer) = self.writer.lock() {
                    let _ = writeln!(writer, "{serialized}");
                }
            }
            LogFormat::Text => {
                let event_prefix = visitor
                    .event_name
                    .map(|n| format!("[{n}] "))
                    .unwrap_or_default();
                let mut attrs_str = String::new();
                for (k, v) in &visitor.attributes {
                    let val_str = match v {
                        Value::String(s) => s.as_str(),
                        Value::Bool(b) => {
                            if *b {
                                "true"
                            } else {
                                "false"
                            }
                        }
                        Value::Number(n) => {
                            attrs_str.push_str(&format!(" {k}={n}"));
                            continue;
                        }
                        _ => "[complex]",
                    };
                    attrs_str.push_str(&format!(" {k}={val_str}"));
                }
                if let Ok(mut writer) = self.writer.lock() {
                    let _ = writeln!(
                        writer,
                        "[{timestamp} {level} {}] {event_prefix}{message}{attrs_str}",
                        self.service_name
                    );
                }
            }
        }
    }
}
