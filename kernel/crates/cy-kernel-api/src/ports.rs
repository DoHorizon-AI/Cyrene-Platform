// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/ports.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 内核六边形架构核心端口 Trait 契约定义.

use crate::{
    binding::DeviceBinding,
    capability::{EnforcementMode, EnforcementReport, NodeCapabilities},
    error::ProviderError,
    inventory::{HealthReport, InventorySnapshot},
    launch::LaunchPlan,
    lease::{ResourceLease, ResourceRequest},
    runtime::{CgroupTelemetry, CleanupReport, ProcessHandle, RuntimeProcessEvidence, StopRequest},
};

/// 端口 Trait 1：硬件资源租约与锁管理器
pub trait ResourceLeaseManager: Send + Sync {
    /// 获取当前最新硬件清单快照
    fn inventory(&self) -> InventorySnapshot;
    /// 以经过边界校验的节点快照刷新可分配库存。
    ///
    /// 实现必须保留已分配设备的可释放记录，并拒绝发生回退的事实代数。
    fn refresh_inventory(&self, snapshot: InventorySnapshot) -> Result<(), ProviderError>;
    /// 申请预留硬件资源租约
    fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError>;
    /// 读取租约当前状态，用于启动/停止流程的 fencing 与结果回报
    fn get_lease(&self, lease_name: &str) -> Result<ResourceLease, ProviderError>;
    /// 使用当前 fencing authority 延长一个活跃租约。续约绝不改变已分配
    /// 资源、持有者或 fence token；实现必须拒绝回退的过期时间。
    fn renew(
        &self,
        lease_name: &str,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<ResourceLease, ProviderError>;
    /// Records the authority decision to release a lease while keeping every
    /// physical resource unavailable until runtime cleanup is confirmed.
    fn begin_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError>;
    /// Completes a release only after the runtime has confirmed physical
    /// cleanup. This is the only operation that makes resources reusable.
    fn complete_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError>;
    /// Records that cleanup could not be confirmed. The allocation remains
    /// held so a failed cleanup can never be mistaken for a released resource.
    fn fail_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError>;
    /// Forcefully removes a lease's authority when its holder can no longer be
    /// trusted. Revoke is distinct from owner-initiated release: it advances
    /// the fence and retains the physical allocation until cleanup is proven.
    fn revoke(&self, lease_name: &str, fence_token: u64) -> Result<ResourceLease, ProviderError>;
    /// Makes a revoked allocation reusable only after the runtime has
    /// confirmed that the old holder cannot retain physical access.
    fn complete_revocation(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError>;
    /// Lists all tracked resource leases for authority and expiry reconciliation.
    fn leases(&self) -> Vec<ResourceLease> {
        Vec::new()
    }
    /// Returns true if physical allocations remain held for this lease.
    fn is_allocated(&self, _lease_name: &str) -> bool {
        false
    }
}

/// 端口 Trait 4：沙箱进程运行时生命周期管理
pub trait ProcessRuntime: Send + Sync {
    /// 节点预检：检查宿主机 cgroup、驱动等基础能力
    fn preflight(&self) -> NodeCapabilities;
    /// 在安全沙箱中启动目标进程
    fn launch(
        &self,
        plan: &LaunchPlan,
        binding: &DeviceBinding,
    ) -> Result<ProcessHandle, ProviderError>;
    /// 停止沙箱中的进程并回收清理资源
    fn stop(
        &self,
        handle: &ProcessHandle,
        request: &StopRequest,
    ) -> Result<CleanupReport, ProviderError>;
    /// 读取运行时可提供的物理资源遥测。后端不支持时返回明确错误，绝不估算。
    fn telemetry(&self, _handle: &ProcessHandle) -> Result<CgroupTelemetry, ProviderError> {
        Err(ProviderError::new(
            "process-runtime",
            "TELEMETRY_UNAVAILABLE",
            "this runtime does not expose physical cgroup telemetry",
        ))
    }
}

/// 端口 Trait 5：沙箱隔离后端标识
pub trait SandboxBackend: ProcessRuntime {
    /// 沙箱后端 ID（如 "cgroupv2-linux"）
    fn backend_id(&self) -> &str;

    /// Lists direct sandbox-owned processes for restart recovery. The default
    /// denies recovery rather than guessing what another runtime owns.
    fn discover_recovery_processes(&self) -> Result<Vec<RuntimeProcessEvidence>, ProviderError> {
        Err(ProviderError::new(
            "sandbox-backend",
            "RECOVERY_UNAVAILABLE",
            "this sandbox backend cannot verify restart recovery evidence",
        ))
    }

    /// Terminates only a process whose current sandbox facts exactly match the
    /// persisted local evidence. Implementations must reject every mismatch.
    fn recover_stale_process(
        &self,
        _evidence: &RuntimeProcessEvidence,
    ) -> Result<CleanupReport, ProviderError> {
        Err(ProviderError::new(
            "sandbox-backend",
            "RECOVERY_UNAVAILABLE",
            "this sandbox backend cannot safely recover a stale process",
        ))
    }
}

/// 端口 Trait 6：设备访问权限与隔离映射器
pub trait DeviceMapper: Send + Sync {
    /// 执行设备隔离策略映射
    fn enforce(
        &self,
        binding: &DeviceBinding,
        requested: EnforcementMode,
    ) -> Result<EnforcementReport, ProviderError>;
}

/// 端口 Trait 7：硬件遥测与指标采集器
pub trait TelemetryProvider: Send + Sync {
    /// 读取指定设备的健康状态与指标
    fn read_health(&self, device_id: &str) -> Result<HealthReport, ProviderError>;
}

/// 端口 Trait 8：节点身份与代数标识提供者
pub trait NodeIdentityProvider: Send + Sync {
    /// 获取节点唯一标识 ID
    fn node_id(&self) -> Result<String, ProviderError>;
    /// 获取节点启动代数周期 (Epoch)
    fn node_epoch(&self) -> Result<u64, ProviderError>;
}
