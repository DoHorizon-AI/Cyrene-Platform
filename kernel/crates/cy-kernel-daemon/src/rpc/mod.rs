// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/rpc/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! gRPC 服务实现模块。

pub(crate) mod authority_service;
pub(crate) mod authority_v2_service;
pub(crate) mod kernel_service;
pub(crate) mod lifecycle_service;
pub(crate) mod provider_service;
pub(crate) mod worker_control;
