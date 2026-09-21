//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 init.rs                                                         │
//! │  Module: cy_observability::init                                     │
//! │  Role: Subscriber initialization, explicit stderr writer, filters. │
//! │                                                                     │
//! │  模块职责：初始化 Subscriber，显式重定向 stderr，配置过滤与守卫。      │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fs::OpenOptions;

use thiserror::Error;
use tracing_appender::non_blocking::NonBlockingBuilder;
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer};

use crate::{
    config::ObservabilityConfig, formatter::CyreneLayer, guard::ObservabilityGuard,
    panic_hook::install_panic_hook,
};

/// ════════════════════════════════════════════════════════════════════════
/// Errors occurring during observability initialization.
/// ════════════════════════════════════════════════════════════════════════
#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("invalid observability configuration: {0}")]
    InvalidConfiguration(String),
    #[error("global subscriber already initialized")]
    AlreadyInitialized,
    #[error("io error setting up file sink: {0}")]
    Io(#[from] std::io::Error),
}

/// ════════════════════════════════════════════════════════════════════════
/// Initializes the global Cyrene observability subscriber according to the
/// provided configuration.
///
/// 强制要求：
/// 1. 显式输出到 stderr，严禁默认或静默污染 stdout；
/// 2. 校验配置过滤规则，无效日志级别立即报错；
/// 3. 安装应急 panic hook；
/// 4. 返回 RAII 守卫，确保有界刷新与安全关闭。
///
/// ════════════════════════════════════════════════════════════════════════
pub fn init_observability(config: ObservabilityConfig) -> Result<ObservabilityGuard, ObservabilityError> {
    config
        .validate()
        .map_err(ObservabilityError::InvalidConfiguration)?;

    // 1. Install emergency panic hook for unhandled panics
    install_panic_hook(
        config.service_name.clone(),
        config.service_instance_id.clone(),
    );

    // 2. Parse explicit filter
    let env_filter = EnvFilter::try_new(&config.log_level)
        .map_err(|err| ObservabilityError::InvalidConfiguration(err.to_string()))?;

    // 3. Set up non-blocking writer explicitly directed to stderr
    let (stderr_writer, stderr_guard) = NonBlockingBuilder::default()
        .lossy(config.lossy)
        .buffered_lines_limit(config.queue_size)
        .finish(std::io::stderr());

    let stderr_layer = CyreneLayer::new(&config, stderr_writer).with_filter(env_filter);
    let mut guards = vec![stderr_guard];

    // 4. Optional local file sink (controlled persistence)
    if let Some(ref rolling) = config.rolling_file {
        let rolling_sink = crate::sink::BoundedRollingFileSink::new(rolling.clone())?;
        let (file_writer, file_guard) = NonBlockingBuilder::default()
            .lossy(config.lossy)
            .buffered_lines_limit(config.queue_size)
            .finish(rolling_sink);
        guards.push(file_guard);

        let file_filter = EnvFilter::try_new(&config.log_level)
            .map_err(|err| ObservabilityError::InvalidConfiguration(err.to_string()))?;
        let file_layer = CyreneLayer::new(&config, file_writer).with_filter(file_filter);

        let subscriber = tracing_subscriber::registry()
            .with(stderr_layer)
            .with(file_layer);

        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| ObservabilityError::AlreadyInitialized)?;
    } else if let Some(ref path) = config.file_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let (file_writer, file_guard) = NonBlockingBuilder::default()
            .lossy(config.lossy)
            .buffered_lines_limit(config.queue_size)
            .finish(file);
        guards.push(file_guard);

        let file_filter = EnvFilter::try_new(&config.log_level)
            .map_err(|err| ObservabilityError::InvalidConfiguration(err.to_string()))?;
        let file_layer = CyreneLayer::new(&config, file_writer).with_filter(file_filter);

        let subscriber = tracing_subscriber::registry()
            .with(stderr_layer)
            .with(file_layer);

        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| ObservabilityError::AlreadyInitialized)?;
    } else {
        let subscriber = tracing_subscriber::registry().with(stderr_layer);

        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| ObservabilityError::AlreadyInitialized)?;
    }

    Ok(ObservabilityGuard::new(guards, config.flush_timeout))
}
