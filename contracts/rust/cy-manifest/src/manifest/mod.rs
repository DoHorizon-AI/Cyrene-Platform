// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Platform manifest projections.
//!
//! This module contains only provider-neutral Artifact identity and the
//! capability discovery records used by Platform control-plane resolution.

pub(crate) mod artifact;
pub(crate) mod plugin;

pub use artifact::*;
pub use plugin::*;
