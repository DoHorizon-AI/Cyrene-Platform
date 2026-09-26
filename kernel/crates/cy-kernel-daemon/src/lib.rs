// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CYRENE 节点内核守护进程组合根 (Kernel Daemon Composition Root).
//!
//! 【内核守护进程职责与设计哲学】
//! 本模块作为节点内核守护进程的装配中心（Composition Root），负责将硬件探测、资源租约与沙箱运行时等各个六边形端口组合连接：
//! 1. **事实汇聚与映射**：聚合底层适配器上报的不可变硬件事实，将其严格、诚实地映射为 Core v1 Protobuf 消息（[`core_v1::KernelCapabilities`]）；
//! 2. **绝不猜测（No Guessing Invariant）**：对于探测不到的显存容量、NUMA 节点或拓扑链路，严格上报未知，绝不用启发式猜测伪造数据；
//! 3. **职责边界**：内核守护进程专职负责单机节点物理事实与资源隔离，不包含远程制品下载、全局调度仲裁或跨重启接管僵尸进程的逻辑。

#![cfg_attr(not(test), forbid(unsafe_code))]
// Tonic owns the concrete Status representation; service helpers keep the
// canonical Result<T, Status> signature instead of boxing transport errors.
// 中文：具体的 Status 表示由 Tonic 管理；服务辅助函数保留规范的 `Result<T, Status>` 签名，而不对传输错误装箱。
#![allow(clippy::result_large_err)]

pub(crate) mod adapter;
pub mod authority;
pub(crate) mod convert;
pub(crate) mod daemon;
pub mod peer_cred;
pub(crate) mod rpc;
pub(crate) mod sandboxed_process;
pub mod service_manager;
pub(crate) mod session;
pub mod watchdog;

#[cfg(test)]
mod tests;

pub use adapter::KernelServiceAdapter;
pub use authority::LocalKernelAuthority;
pub use daemon::KernelDaemon;
pub use service_manager::ServiceSupervisionManager;
pub use session::WorkerHeartbeatConfig;
pub use watchdog::{InstanceActor, InstanceActorState, InstanceHealthVerdict, ServiceSupervisor};
