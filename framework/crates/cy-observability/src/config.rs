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
/// 中文：诊断日志的输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Strict single-line NDJSON format conforming to Cyrene specification.
    /// 中文：符合 Cyrene 规范的严格单行 NDJSON 格式。
    Json,
    /// Human-readable text format for local interactive development.
    /// 中文：供本地交互式开发使用的易读文本格式。
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
/// 中文：Cyrene observability subscriber 的配置。
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    /// Logical service name (e.g. "cyrene-kernel", "cy-node-agent").
    /// 中文：逻辑 Service 名称，例如 cyrene-kernel 或 cy-node-agent。
    pub service_name: String,
    /// Unique instance identifier for this process run.
    /// 中文：此进程运行实例的唯一标识符。
    pub service_instance_id: String,
    /// Log output format (JSON or development text).
    /// 中文：日志输出格式（JSON 或开发用文本）。
    pub format: LogFormat,
    /// Explicit log level or directive filter (e.g. "info", "debug").
    /// 中文：显式日志级别或 directive 过滤器，例如 info 或 debug。
    pub log_level: String,
    /// Optional path to a local persistent log file sink.
    /// 中文：本地持久化日志文件 sink 的可选路径。
    pub file_path: Option<PathBuf>,
    /// Optional controlled rolling file persistence configuration.
    /// 中文：受控滚动文件持久化配置（可选）。
    pub rolling_file: Option<crate::sink::RollingFileConfig>,
    /// Bounded in-memory queue record budget.
    /// 中文：有界内存队列可容纳的记录数。
    pub queue_size: usize,
    /// Maximum time to wait for bounded flush on shutdown.
    /// 中文：关闭时等待有界 flush 的最长时长。
    pub flush_timeout: Duration,
    /// Maximum allowed bytes per JSON record.
    /// 中文：每条 JSON 记录允许的最大字节数。
    pub max_record_bytes: usize,
    /// Maximum allowed bytes per message string.
    /// 中文：消息字符串允许的最大字节数。
    pub max_message_bytes: usize,
    /// Whether the worker queue is lossy when full.
    /// 中文：队列已满时是否允许丢弃记录。
    pub lossy: bool,
}

impl ObservabilityConfig {
    /// ════════════════════════════════════════════════════════════════════
    /// Creates a production managed configuration with NDJSON and stderr.
    /// ════════════════════════════════════════════════════════════════════
    /// 中文：创建生产环境使用的受管理配置，采用 NDJSON 格式并写入 stderr。
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
    /// 中文：创建供交互式开发使用的配置，采用文本格式。
    pub fn development(service_name: impl Into<String>) -> Self {
        let mut config = Self::managed(service_name);
        config.format = LogFormat::Text;
        config.log_level = "debug".to_string();
        config
    }

    /// Set explicit instance ID.
    /// 中文：设置显式 instance ID。
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self {
        self.service_instance_id = instance_id.into();
        self
    }

    /// Set explicit log format.
    /// 中文：设置显式日志格式。
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    /// Set explicit log level filter.
    /// 中文：设置显式日志级别过滤器。
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// Set optional file path for controlled file sink.
    /// 中文：设置受控文件 sink 的可选路径。
    pub fn with_file_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.file_path = Some(path.into());
        self
    }

    /// Set controlled rolling file persistence configuration.
    /// 中文：设置受控滚动文件持久化配置。
    pub fn with_rolling_file(mut self, rolling: crate::sink::RollingFileConfig) -> Self {
        self.rolling_file = Some(rolling);
        self
    }

    /// Set bounded queue size.
    /// 中文：设置有界队列大小。
    pub fn with_queue_size(mut self, size: usize) -> Self {
        self.queue_size = size;
        self
    }

    /// Set bounded flush timeout.
    /// 中文：设置有界 flush 超时时长。
    pub fn with_flush_timeout(mut self, timeout: Duration) -> Self {
        self.flush_timeout = timeout;
        self
    }

    /// Set whether the queue allows dropping on overflow.
    /// 中文：设置队列溢出时是否允许丢弃记录。
    pub fn with_lossy(mut self, lossy: bool) -> Self {
        self.lossy = lossy;
        self
    }

    /// ════════════════════════════════════════════════════════════════════
    /// Strictly validates the configuration against spec requirements.
    ///
    /// 中文：依据规范要求严格校验配置；无效日志级别不得静默回退，必须直接报错。
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
        // 中文：严格验证日志级别语法：只能是标准级别词或有效 directive。
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
