//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 config.rs                                                       │
//! │  Module: cy_observability::config                                   │
//! │  Role: Configuration structures, profile parsing, and validation.   │
//! │                                                                     │
//! │  模块职责：可观测性配置结构、运行 Profile 解析与严格校验。              │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{path::PathBuf, str::FromStr, time::Duration};

use crate::redaction::{DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_MAX_RECORD_BYTES};

/// ════════════════════════════════════════════════════════════════════════
/// Output format for diagnostic logging.
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Strict single-line NDJSON format conforming to Cyrene specification.
    Json,
    /// Human-readable text format for local interactive development.
    Text,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" | "ndjson" => Ok(Self::Json),
            "text" | "pretty" | "console" => Ok(Self::Text),
            other => Err(format!(
                "invalid log format '{other}', expected 'json' or 'text'"
            )),
        }
    }
}

/// ════════════════════════════════════════════════════════════════════════
/// Configuration for the Cyrene observability subscriber.
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    /// Logical service name (e.g. "cyrene-kernel", "cy-node-agent").
    pub service_name: String,
    /// Unique instance identifier for this process run.
    pub service_instance_id: String,
    /// Log output format (JSON or development text).
    pub format: LogFormat,
    /// Explicit log level or directive filter (e.g. "info", "debug").
    pub log_level: String,
    /// Optional path to a local persistent log file sink.
    pub file_path: Option<PathBuf>,
    /// Optional controlled rolling file persistence configuration.
    pub rolling_file: Option<crate::sink::RollingFileConfig>,
    /// Bounded in-memory queue record budget.
    pub queue_size: usize,
    /// Maximum time to wait for bounded flush on shutdown.
    pub flush_timeout: Duration,
    /// Maximum allowed bytes per JSON record.
    pub max_record_bytes: usize,
    /// Maximum allowed bytes per message string.
    pub max_message_bytes: usize,
    /// Whether the worker queue is lossy when full.
    pub lossy: bool,
}

impl ObservabilityConfig {
    /// ════════════════════════════════════════════════════════════════════
    /// Creates a production managed configuration with NDJSON and stderr.
    /// ════════════════════════════════════════════════════════════════════
    pub fn managed(service_name: impl Into<String>) -> Self {
        let name = service_name.into();
        let instance_id = uuid::Uuid::new_v4().to_string();
        Self {
            service_name: name,
            service_instance_id: instance_id,
            format: LogFormat::Json,
            log_level: "info".to_string(),
            file_path: None,
            rolling_file: None,
            queue_size: 10_000,
            flush_timeout: Duration::from_secs(2),
            max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            lossy: false,
        }
    }

    /// ════════════════════════════════════════════════════════════════════
    /// Creates an interactive development configuration with text format.
    /// ════════════════════════════════════════════════════════════════════
    pub fn development(service_name: impl Into<String>) -> Self {
        let mut config = Self::managed(service_name);
        config.format = LogFormat::Text;
        config.log_level = "debug".to_string();
        config
    }

    /// Set explicit instance ID.
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self {
        self.service_instance_id = instance_id.into();
        self
    }

    /// Set explicit log format.
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    /// Set explicit log level filter.
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// Set optional file path for controlled file sink.
    pub fn with_file_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.file_path = Some(path.into());
        self
    }

    /// Set controlled rolling file persistence configuration.
    pub fn with_rolling_file(mut self, rolling: crate::sink::RollingFileConfig) -> Self {
        self.rolling_file = Some(rolling);
        self
    }

    /// Set bounded queue size.
    pub fn with_queue_size(mut self, size: usize) -> Self {
        self.queue_size = size;
        self
    }

    /// Set bounded flush timeout.
    pub fn with_flush_timeout(mut self, timeout: Duration) -> Self {
        self.flush_timeout = timeout;
        self
    }

    /// Set whether the queue allows dropping on overflow.
    pub fn with_lossy(mut self, lossy: bool) -> Self {
        self.lossy = lossy;
        self
    }

    /// ════════════════════════════════════════════════════════════════════
    /// Strictly validates the configuration against spec requirements.
    ///
    /// 严格校验配置。无效日志级别不可静默回退，必须直接报错。
    /// ════════════════════════════════════════════════════════════════════
    pub fn validate(&self) -> Result<(), String> {
        if self.service_name.trim().is_empty() {
            return Err("service_name cannot be empty".to_string());
        }
        if self.service_instance_id.trim().is_empty() {
            return Err("service_instance_id cannot be empty".to_string());
        }
        if self.queue_size == 0 {
            return Err("queue_size must be greater than zero".to_string());
        }
        // Validate log level syntax strictly: either standard level word or valid directive
        let trimmed = self.log_level.trim();
        if !trimmed.contains('=') && !trimmed.contains(',') {
            match trimmed.to_ascii_lowercase().as_str() {
                "trace" | "debug" | "info" | "warn" | "error" | "off" => {}
                other => {
                    return Err(format!(
                        "invalid log level '{other}'; must be trace, debug, info, warn, error, off, or target=level directives"
                    ));
                }
            }
        }
        tracing_subscriber::EnvFilter::try_new(&self.log_level)
            .map_err(|err| format!("invalid log filter syntax '{}': {}", self.log_level, err))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_configs_pass_validation() {
        let config = ObservabilityConfig::managed("cyrene-kernel");
        assert!(config.validate().is_ok());

        let dev = ObservabilityConfig::development("cy-node-agent");
        assert!(dev.validate().is_ok());
    }

    #[test]
    fn invalid_log_level_fails_validation() {
        let invalid =
            ObservabilityConfig::managed("test").with_log_level("invalid_level_syntax!!!");
        let result = invalid.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid log level"));
    }

    #[test]
    fn empty_service_name_fails_validation() {
        let invalid = ObservabilityConfig::managed("   ");
        assert!(invalid.validate().is_err());
    }
}
