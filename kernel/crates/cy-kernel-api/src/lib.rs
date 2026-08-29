//! CYRENE 节点内核核心 API 契约与端口定义 (Kernel Core Ports & Fact Types).
//!
//! 【六边形架构与端口-适配器模式 (Hexagonal Architecture)】
//! 本 Crate 作为节点内核的核心抽象契约层，**刻意不包含任何特定 Linux 系统调用、硬件厂商 SDK、gRPC (Tonic) 或底层进程实现的细节**。
//! 通用端口的实现（如进程外硬件适配器客户端、cgroup v2 沙箱、内存租约管理器）向内核上报事实（Facts），
//! 内核守护进程组合根将其组装暴露为统一的 Core v1 平台服务：
//!
//! - **资源与事实**：[`semantic::Resource`], [`semantic::Capability`], [`DeviceNode`], [`HealthReport`]
//! - **资源租约与围栏**：[`ResourceLease`], [`ResourceRequest`], [`DeviceBinding`], [`EnforcementMode`]
//! - **沙箱与进程生命周期**：[`LaunchPlan`], [`ProcessHandle`], [`CleanupReport`], [`StopRequest`]
//! - **核心端口 Trait**：[`ResourceProvider`], [`ResourceLeaseManager`], [`ProcessRuntime`], [`SandboxBackend`] 等

#![cfg_attr(not(test), forbid(unsafe_code))]

pub mod authority;
pub mod binding;
pub mod capability;
pub mod error;
pub mod inventory;
pub mod journal;
pub mod launch;
pub mod lease;
pub mod ports;
pub mod runtime;
pub mod service;

/// Canonical, transport-independent Kernel vocabulary. New public ports must
/// use these nouns; the remaining device/process structs below are internal
/// compatibility and adapter-port types during migration.
pub use cy_kernel_contract as semantic;

// 扁平导出各子模块中的核心类型与 Trait，保持 100% 向后兼容
pub use authority::{
    AuthorityCallContext, AuthoritySnapshot, KernelAuthority, KernelProviderAuthority, NamespaceId,
    ObjectRef, ProviderReconcileAction, ProviderReconcileResult, DEFAULT_NAMESPACE,
};
pub use binding::{DeviceBinding, EnvironmentMerge};
pub use capability::{CapabilityFact, EnforcementMode, EnforcementReport, NodeCapabilities};
pub use error::ProviderError;
pub use inventory::{DeviceNode, HealthReport, InventorySnapshot};
pub use journal::{
    DurableEventRecord, DurableEventStore, FailingRuntimeJournal, NoopRuntimeJournal,
    RuntimeJournalEvent, RuntimeJournalRecord, RuntimeJournalSink,
};
pub use launch::{InstalledPluginResolver, LaunchPlan, ResolvedLaunchPlan, VerifiedInstallation};
pub use lease::{CgroupLimits, LeaseState, ResourceAllocation, ResourceLease, ResourceRequest};
pub use ports::{
    DeviceMapper, HostInventoryProvider, NodeIdentityProvider, ProcessRuntime,
    ResourceLeaseManager, ResourceProvider, SandboxBackend, TelemetryProvider,
};
pub use runtime::{
    CgroupTelemetry, CleanupReport, ProcessCondition, ProcessHandle, RuntimeProcessEvidence,
    StopRequest,
};
pub use service::{
    BackoffConfig, ProbeConfig, ReadinessProbe, RestartPolicy, ServiceEndpointSpec, ServiceEvent,
    ServiceSpec, ServiceState, ServiceStatus,
};
