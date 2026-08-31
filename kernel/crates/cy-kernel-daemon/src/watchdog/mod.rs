// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/watchdog/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Instance Watchdog Actor Spike for CYRENE Kernel.
//!
//! Owns an instance-level execution actor that manages immutable launch plans,
//! resource lease fencing tokens, heartbeat deadline checks, crash-loop quarantine,
//! and bounded sandbox lifecycle.

mod instance_actor;
pub mod supervisor;

pub use instance_actor::{
    InstanceActor, InstanceActorState, InstanceHealthVerdict, WorkerCancelAck,
    WorkerTransportCommand, WorkerTransportDispatcher, WorkerTransportRequest,
    WorkerTransportResponse,
};
pub use supervisor::ServiceSupervisor;
