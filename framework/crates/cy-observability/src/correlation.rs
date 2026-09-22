//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 correlation.rs                                                  │
//! │  Module: cy_observability::correlation                              │
//! │  Role: Correlation hierarchy, W3C Trace Context, and sanitization.  │
//! │                                                                     │
//! │  模块职责：关联标识分层模型、W3C Trace Context 解析与不可信边界清洗。   │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fmt;
use thiserror::Error;

/// Maximum allowed length for a request_id.
pub const MAX_REQUEST_ID_LEN: usize = 128;
/// Maximum allowed length for an operation_id.
pub const MAX_OPERATION_ID_LEN: usize = 128;
/// Maximum allowed length for a resource_id.
pub const MAX_RESOURCE_ID_LEN: usize = 256;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CorrelationError {
    #[error("invalid traceparent format: expected 4 hyphen-separated parts (version-traceid-spanid-flags)")]
    InvalidFormat,
    #[error("unsupported traceparent version: {0} (expected 00)")]
    UnsupportedVersion(String),
    #[error("invalid trace_id: must be 32 lowercase hex characters and non-zero")]
    InvalidTraceId,
    #[error("invalid span_id: must be 16 lowercase hex characters and non-zero")]
    InvalidSpanId,
    #[error("invalid trace_flags: must be 2 hex characters")]
    InvalidFlags,
}

/// ════════════════════════════════════════════════════════════════════════
/// W3C Trace Context representation (traceparent header).
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub flags: u8,
}

impl TraceContext {
    /// Generates a new root trace context with random trace_id and span_id.
    pub fn new_root() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos();
        let pid = std::process::id() as u128;

        // Generate non-zero pseudorandom bytes without requiring heavy rand crate
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];

        let t_bytes = (nanos ^ (pid << 64) ^ 0x5a5a_3c3c_9696_c3c3).to_be_bytes();
        trace_id.copy_from_slice(&t_bytes);
        // Ensure non-zero
        if trace_id == [0u8; 16] {
            trace_id[15] = 1;
        }

        let s_bytes = ((nanos >> 32) ^ pid ^ 0xa5a5_c3c3).to_be_bytes();
        span_id.copy_from_slice(&s_bytes[..8]);
        if span_id == [0u8; 8] {
            span_id[7] = 1;
        }

        Self {
            trace_id,
            span_id,
            flags: 0x01, // Sampled
        }
    }

    /// Creates a child span context under the same trace_id.
    pub fn child_span(&self) -> Self {
        let mut child = Self::new_root();
        child.trace_id = self.trace_id;
        child.flags = self.flags;
        child
    }

    /// Parses a standard W3C `traceparent` header string.
    ///
    /// Expected format: `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`
    pub fn parse_traceparent(raw: &str) -> Result<Self, CorrelationError> {
        let trimmed = raw.trim();
        let parts: Vec<&str> = trimmed.split('-').collect();
        if parts.len() != 4 {
            return Err(CorrelationError::InvalidFormat);
        }

        let version = parts[0];
        if version != "00" {
            return Err(CorrelationError::UnsupportedVersion(version.to_string()));
        }

        let trace_id_str = parts[1];
        if trace_id_str.len() != 32 || !trace_id_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CorrelationError::InvalidTraceId);
        }
        let mut trace_id = [0u8; 16];
        for i in 0..16 {
            trace_id[i] = u8::from_str_radix(&trace_id_str[i * 2..i * 2 + 2], 16)
                .map_err(|_| CorrelationError::InvalidTraceId)?;
        }
        if trace_id == [0u8; 16] {
            return Err(CorrelationError::InvalidTraceId);
        }

        let span_id_str = parts[2];
        if span_id_str.len() != 16 || !span_id_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CorrelationError::InvalidSpanId);
        }
        let mut span_id = [0u8; 8];
        for i in 0..8 {
            span_id[i] = u8::from_str_radix(&span_id_str[i * 2..i * 2 + 2], 16)
                .map_err(|_| CorrelationError::InvalidSpanId)?;
        }
        if span_id == [0u8; 8] {
            return Err(CorrelationError::InvalidSpanId);
        }

        let flags_str = parts[3];
        if flags_str.len() != 2 || !flags_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CorrelationError::InvalidFlags);
        }
        let flags =
            u8::from_str_radix(flags_str, 16).map_err(|_| CorrelationError::InvalidFlags)?;

        Ok(Self {
            trace_id,
            span_id,
            flags,
        })
    }

    /// Returns the trace_id formatted as a 32-character lowercase hex string.
    pub fn trace_id_hex(&self) -> String {
        let mut s = String::with_capacity(32);
        for byte in &self.trace_id {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }

    /// Returns the span_id formatted as a 16-character lowercase hex string.
    pub fn span_id_hex(&self) -> String {
        let mut s = String::with_capacity(16);
        for byte in &self.span_id {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }

    /// Formats this context as a standard W3C `traceparent` header string.
    pub fn to_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id_hex(),
            self.span_id_hex(),
            self.flags
        )
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_traceparent())
    }
}

/// ════════════════════════════════════════════════════════════════════════
/// Untrusted correlation header sanitizer.
///
/// Ensures untrusted client headers cannot inject control characters,
/// newlines, or oversized payloads into structured logging sinks.
/// ════════════════════════════════════════════════════════════════════════
pub fn sanitize_correlation_id(raw: &str, max_len: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Filter out any control characters, newlines, tabs, carriage returns, or quotes
    let filtered: String = trimmed
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || *c == '-'
                || *c == '_'
                || *c == '.'
                || *c == '/'
                || *c == ':'
        })
        .collect();

    if filtered.is_empty() {
        return None;
    }

    if filtered.len() > max_len {
        Some(filtered[..max_len].to_string())
    } else {
        Some(filtered)
    }
}

/// Sanitizes an incoming `request_id` header with a 128-character bound.
pub fn sanitize_request_id(raw: &str) -> Option<String> {
    sanitize_correlation_id(raw, MAX_REQUEST_ID_LEN)
}

/// Sanitizes an incoming `operation_id` header with a 128-character bound.
pub fn sanitize_operation_id(raw: &str) -> Option<String> {
    sanitize_correlation_id(raw, MAX_OPERATION_ID_LEN)
}

/// Sanitizes an incoming `resource_id` reference with a 256-character bound.
pub fn sanitize_resource_id(raw: &str) -> Option<String> {
    sanitize_correlation_id(raw, MAX_RESOURCE_ID_LEN)
}

/// ════════════════════════════════════════════════════════════════════════
/// Full Correlation Context maintaining strict hierarchy between
/// request, long-running operation, business resource, and distributed trace.
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorrelationContext {
    pub request_id: Option<String>,
    pub operation_id: Option<String>,
    pub resource_id: Option<String>,
    pub trace_context: Option<TraceContext>,
}

impl CorrelationContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = sanitize_request_id(&request_id.into());
        self
    }

    pub fn with_operation_id(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = sanitize_operation_id(&operation_id.into());
        self
    }

    pub fn with_resource_id(mut self, resource_id: impl Into<String>) -> Self {
        self.resource_id = sanitize_resource_id(&resource_id.into());
        self
    }

    pub fn with_trace_context(mut self, trace_context: TraceContext) -> Self {
        self.trace_context = Some(trace_context);
        self
    }

    pub fn parse_w3c_traceparent(mut self, header_value: &str) -> Self {
        if let Ok(tc) = TraceContext::parse_traceparent(header_value) {
            self.trace_context = Some(tc);
        }
        self
    }
}
