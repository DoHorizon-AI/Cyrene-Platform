// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Internal Platform management and capability-selection library.
//!
//! Capability payload contracts and concrete implementations belong to the
//! Plugins repository or their Product owner. This crate exposes only generic
//! package, resolution, lifecycle, and opaque endpoint facts; it
//! must not grow capability-specific traits or payload models. This crate
//! contains registry, resolver, and repository-normalization implementation
//! logic; it is not the supported Apache public extension API. External
//! extensions use `cy-manifest`, `cy-proto`, or a dedicated SDK instead.

pub mod plugin;
pub mod repository_manifest;
pub use cy_manifest::*;
pub use plugin::{
    CapabilityBinding, CapabilityRegistry, CapabilityResolutionError, CapabilityResolver,
    ResolvedCapabilityTarget,
};
pub use repository_manifest::{RepositoryPluginManifest, normalize_repository_manifest};
