// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Product-neutral Platform management and worker-lifecycle API.
//!
//! Capability payload contracts and concrete implementations belong to the
//! Plugins repository or their Product owner. This crate exposes only generic
//! package, resolution, lifecycle, and opaque worker transport primitives; it
//! must not grow capability-specific traits or payload models.

pub mod plugin;
pub mod repository_manifest;
pub mod worker;
pub use cy_manifest::*;
pub use plugin::{
    CapabilityBinding, CapabilityRegistry, CapabilityResolutionError, CapabilityResolver,
    ResolvedCapabilityTarget,
};
pub use repository_manifest::{RepositoryPluginManifest, normalize_repository_manifest};
pub use worker::{
    ApplicationEventError, ApplicationEventStreamEndReason, ApplicationEventStreamTermination,
    ApplicationEventSubscription, AtomicCancellationToken, CancellationToken,
    CapabilityWorkerActivator, CapabilityWorkerClient, DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY,
    MAX_APPLICATION_EVENT_BUFFER_CAPACITY, NeverCancelled, WorkerActivationOptions,
    WorkerApplicationEvent, WorkerInvocationResult, WorkerTerminalError,
};
