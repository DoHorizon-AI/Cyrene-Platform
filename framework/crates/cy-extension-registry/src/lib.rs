//! CYRENE 扩展注册中心与远程 RPC 代理层 (Extension Registry & Remote Proxies).
//!
//! 【扩展代理与注册中心架构】
//! 为平台 10 大扩展点提供类型安全的远程 RPC 代理与统一查找管理：
//! 1. **强类型远程代理 (Typed Remote Proxies)**：
//!    将平台 SPI Trait（如 [`Probe`], [`ModelAnalyzer`], [`ExecutionEngine`]）的异步方法透明转换为
//!    Protobuf [`Invoke`] 封包，经由 [`InstanceActor`] 的 sandboxd 通道与插件子进程交互；
//! 2. **前置健康检查与自愈重连 ([`prepare_instance_actor`])**：
//!    在发起 RPC 调用前，自动调用 `InstanceActor::ensure_healthy`，若插件处于崩溃态且策略允许则自动
//!    触发指数退避重启；
//! 3. **强类型注册表 ([`ExtensionRegistry`])**：
//!    按扩展点分类（`Probe`, `Storage`, `Notification` 等）分别索引并提供类型安全的高效注册、按 ID 查询
//!    与批量枚举能力。

pub(crate) mod helper;
pub(crate) mod proxies;
pub(crate) mod registry;

#[cfg(test)]
mod tests;

// 扁平导出核心公共类型与辅助函数
pub use helper::prepare_instance_actor;
pub use proxies::*;
pub use registry::ExtensionRegistry;
