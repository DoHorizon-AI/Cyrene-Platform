// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CYRENE 节点内核内部端口与实现事实 (Kernel Internal Ports & Fact Types).
//!
//! 【六边形架构与端口-适配器模式 (Hexagonal Architecture)】
//! 本 Crate 是 Platform Core 的 implementation-facing seam，不是第三方插件
//! API，也不是 Apache 公共 SDK。它刻意不包含特定 Linux 系统调用、硬件厂商
//! SDK、gRPC (Tonic) 或底层进程实现的细节，但包含内核编排、租约、journal、
//! sandbox 与 runtime 端口。
//! 通用端口的实现（如进程外硬件适配器客户端、cgroup v2 沙箱、内存租约管理器）向内核上报事实（Facts），
//! 内核守护进程组合根将其组装暴露为统一的 Core v1 平台服务：
//!
//! - **公共适配器事实**：[`cy_kernel_contract`] (本 crate 只保留兼容导出)
//! - **资源租约与围栏**：[`ResourceLease`], [`ResourceRequest`], [`DeviceBinding`], [`EnforcementMode`]
//! - **沙箱与进程生命周期**：[`LaunchPlan`], [`ProcessHandle`], [`CleanupReport`], [`StopRequest`]
//! - **内核端口 Trait**：[`ResourceLeaseManager`], [`ProcessRuntime`], [`SandboxBackend`] 等

#![cfg_attr(not(test), forbid(unsafe_code))]

pub mod authority;
pub mod binding;
pub mod capability;
pub mod error;
pub mod inventory;
pub mod journal;
pub mod launch;
pub mod lease;
#[doc(hidden)]
pub mod ports;
pub mod runtime;
pub mod service;

/// Canonical, transport-independent Kernel vocabulary. New public ports must
/// use these nouns; the remaining device/process structs below are internal
/// compatibility and adapter-port types during migration.
pub use cy_kernel_contract as semantic;

// Internal crates retain the historical flat names. Third-party extensions
// must use cy-kernel-contract and cy-proto instead.
pub use authority::{
    AuthorityCallContext, AuthoritySnapshot, KernelAuthority, KernelProviderAuthority, NamespaceId,
    ObjectRef, ProviderReconcileAction, ProviderReconcileResult, DEFAULT_NAMESPACE,
};
pub use binding::{DeviceBinding, EnvironmentMerge};
pub use capability::{CapabilityFact, EnforcementMode, EnforcementReport, NodeCapabilities};
#[doc(hidden)]
pub use cy_kernel_contract::{HostInventoryProvider, ResourceProvider};
pub use error::ProviderError;
pub use inventory::{DeviceNode, HealthReport, InventorySnapshot};
pub use journal::{
    DurableEventRecord, DurableEventStore, FailingRuntimeJournal, NoopRuntimeJournal,
    RuntimeJournalEvent, RuntimeJournalRecord, RuntimeJournalSink,
};
pub use launch::{InstalledPluginResolver, LaunchPlan, ResolvedLaunchPlan, VerifiedInstallation};
pub use lease::{CgroupLimits, LeaseState, ResourceAllocation, ResourceLease, ResourceRequest};
#[doc(hidden)]
pub use ports::{
    DeviceMapper, NodeIdentityProvider, ProcessRuntime, ResourceLeaseManager, SandboxBackend,
    TelemetryProvider,
};
pub use runtime::{
    CgroupTelemetry, CleanupReport, ProcessCondition, ProcessHandle, RuntimeProcessEvidence,
    StopRequest,
};
pub use service::{
    BackoffConfig, ProbeConfig, ReadinessProbe, RestartPolicy, ServiceEndpointSpec, ServiceEvent,
    ServiceSpec, ServiceState, ServiceStatus,
};
