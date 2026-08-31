// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-adapter-client/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Kernel-side client for external hardware adapter processes.
//!
//! This crate owns only a bounded, versioned Unix-domain-socket exchange.  It
//! contains no driver command, vendor library, sysfs probe, or device-node
//! knowledge.  A disconnected adapter is reported as a provider failure so the
//! caller can stop issuing new leases while preserving its existing state.

#![cfg_attr(not(test), forbid(unsafe_code))]

pub(crate) mod client;
pub(crate) mod convert;
pub(crate) mod credential;
pub(crate) mod registry;
pub(crate) mod transport;

#[cfg(test)]
mod tests;

pub use client::{
    HardwareAdapter, HardwareAdapterEndpoint, HardwareAdapterObservation, HostAdapterClient,
    UdsHardwareAdapterClient,
};
pub use convert::resource_from_proto;
pub use credential::PeerCredentialExpectation;
pub use registry::UdsHardwareAdapterRegistry;
pub use transport::{read_frame, write_frame, MAX_FRAME_BYTES, PROTOCOL_VERSION};
