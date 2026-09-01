// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-extension-registry/src/proxies/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 平台 10 大扩展点与通用插件的远程 RPC 代理实现

pub mod compat_rule;
pub mod execution_engine;
pub mod gateway_filter;
pub mod generic;
pub mod model_analyzer;
pub mod notification;
pub mod probe;
pub mod quantization;
pub mod runtime_builder;
pub mod storage;
pub mod training_backend;

pub use compat_rule::RemoteCompatRule;
pub use execution_engine::RemoteExecutionEngine;
pub use gateway_filter::RemoteGatewayFilter;
pub use generic::GenericRemotePlugin;
pub use model_analyzer::RemoteModelAnalyzer;
pub use notification::RemoteNotification;
pub use probe::RemoteProbe;
pub use quantization::RemoteQuantization;
pub use runtime_builder::RemoteRuntimeBuilder;
pub use storage::RemoteStorage;
pub use training_backend::RemoteTrainingBackend;

// 扩展代理类型别名 (Proxy Aliases)
pub type ProbeProxy = RemoteProbe;
pub type ModelAnalyzerProxy = RemoteModelAnalyzer;
pub type CompatRuleProxy = RemoteCompatRule;
pub type RuntimeBuilderProxy = RemoteRuntimeBuilder;
pub type ExecutionEngineProxy = RemoteExecutionEngine;
pub type TrainingBackendProxy = RemoteTrainingBackend;
pub type QuantizationProxy = RemoteQuantization;
pub type GatewayFilterProxy = RemoteGatewayFilter;
pub type NotificationProxy = RemoteNotification;
pub type StorageProxy = RemoteStorage;
pub type GenericPluginProxy = GenericRemotePlugin;
