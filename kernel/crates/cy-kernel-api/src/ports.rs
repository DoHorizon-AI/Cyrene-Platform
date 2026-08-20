//! 内核六边形架构核心端口 Trait 契约定义.

use cy_kernel_contract as semantic;

use crate::{
    binding::DeviceBinding,
    capability::{EnforcementMode, EnforcementReport, NodeCapabilities},
    error::ProviderError,
    inventory::{HealthReport, InventorySnapshot},
    launch::LaunchPlan,
    lease::{ResourceLease, ResourceRequest},
    runtime::{CgroupTelemetry, CleanupReport, ProcessHandle, StopRequest},
};

/// 端口 Trait 1：宿主机全量硬件清单探测器
pub trait HostInventoryProvider: Send + Sync {
    /// 采集并返回节点硬件清单快照
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError>;
}

/// 端口 Trait 2：特定厂商硬件加速卡适配器
pub trait ResourceProvider: Send + Sync {
    /// 适配器唯一 ID（例如："hardware-adapter-uds"）
    fn adapter_id(&self) -> &str;
    /// 探测该 Provider 的通用资源事实。
    fn probe_resources(&self) -> Result<Vec<semantic::Resource>, ProviderError>;
    /// 为指定资源创建安全隔离绑定规则。
    fn create_binding(&self, resource: &semantic::Resource)
        -> Result<DeviceBinding, ProviderError>;
    /// 为指定库存代次创建绑定。
    ///
    /// 旧的或纯静态实现可以安全地沿用无代次的默认实现；进程外 Adapter Host
    /// 必须覆盖此方法并将代次传给其协议端点。
    fn create_binding_for_generation(
        &self,
        resource: &semantic::Resource,
        _expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        self.create_binding(resource)
    }
    /// 读取指定资源的健康状态。
    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError>;
}

/// 端口 Trait 3：硬件资源租约与锁管理器
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
