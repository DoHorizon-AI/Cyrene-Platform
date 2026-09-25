// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/daemon.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 节点内核守护进程核心结构与组合根实现。

use std::{collections::BTreeMap, sync::Arc};

use cy_adapter_client::{
    HardwareAdapterEndpoint, HardwareAdapterObservation, UdsHardwareAdapterRegistry,
};
use cy_kernel_api::{
    DeviceBinding, HostInventoryProvider, InventorySnapshot, ProviderError, ResourceLease,
    ResourceLeaseManager, ResourceProvider, ResourceRequest, SandboxBackend,
};
use cy_proto::core_v1;

use crate::convert::{
    merge_bindings, now_timestamp, to_proto_enforcement, to_semantic_proto_resource,
};

/// 节点内核守护进程核心结构体
pub struct KernelDaemon {
    /// 宿主机硬件清单提供者
    pub(crate) inventory_provider: Arc<dyn HostInventoryProvider>,
    /// 进程外通用资源 Provider
    pub(crate) resource_provider: Arc<dyn ResourceProvider>,
    /// Configured hardware adapters remain separately observable for Provider
    /// lifecycle projection; allocation still consumes their aggregate view.
    /// 已配置的硬件适配器仍可分别观测，以便投影 Provider 生命周期；分配仍使用它们的聚合视图。
    pub(crate) hardware_adapters: Option<Arc<UdsHardwareAdapterRegistry>>,
    /// 硬件资源租约管理器
    pub(crate) resources: Arc<dyn ResourceLeaseManager>,
    /// 沙箱隔离后端
    pub(crate) sandbox: Arc<dyn SandboxBackend>,
    /// 节点唯一 ID
    pub(crate) node_id: String,
    /// 节点启动纪元代数 (Epoch)
    pub(crate) node_epoch: u64,
}

impl KernelDaemon {
    /// 构造新的内核守护进程实例
    pub fn new(
        inventory_provider: Arc<dyn HostInventoryProvider>,
        resource_provider: Arc<dyn ResourceProvider>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Self {
        Self {
            inventory_provider,
            resource_provider,
            hardware_adapters: None,
            resources,
            sandbox,
            node_id: node_id.into(),
            node_epoch,
        }
    }

    /// 通过版本化 UDS 连接进程外系统/硬件适配器注册表。
    ///
    /// 该组合根不加载厂商动态库，也不执行任何厂商探测命令。适配器失联时，端口
    /// 返回 `ADAPTER_UNAVAILABLE`，由上层将节点转为不可继续分配的降级状态。
    pub fn with_adapters(
        endpoints: impl IntoIterator<Item = HardwareAdapterEndpoint>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Result<Self, ProviderError> {
        let adapters = Arc::new(UdsHardwareAdapterRegistry::from_endpoints(endpoints)?);
        let hardware_adapters = Arc::clone(&adapters);
        let mut daemon = Self::new(
            adapters.clone(),
            adapters,
            resources,
            sandbox,
            node_id,
            node_epoch,
        );
        daemon.hardware_adapters = Some(hardware_adapters);
        Ok(daemon)
    }

    /// Backward-compatible constructor for callers that only use hardware
    /// resource adapters. The registry also accepts the required system
    /// adapter endpoint used by the Linux composition root.
    /// 向后兼容的构造函数，供只使用硬件资源适配器的调用方使用。注册表也会接收 Linux 组合根所需的系统适配器端点。
    pub fn with_hardware_adapters(
        endpoints: impl IntoIterator<Item = HardwareAdapterEndpoint>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Result<Self, ProviderError> {
        Self::with_adapters(endpoints, resources, sandbox, node_id, node_epoch)
    }

    /// 检查节点基础沙箱环境是否已就绪
    pub fn preflight_ready(&self) -> bool {
        self.sandbox_preflight_ready()
            && self
                .inventory()
                .is_ok_and(|snapshot| snapshot.capabilities.ready)
    }

    /// Runtime admission is independent from hardware fact availability so the
    /// Kernel can project an unavailable hardware Provider instead of hiding
    /// all of its other local authority state at process startup.
    /// runtime 接入与硬件事实是否可用相互独立，因此 Kernel 可以投影不可用的硬件 Provider，而不会在进程启动时隐藏其余本地 authority 状态。
    pub fn sandbox_preflight_ready(&self) -> bool {
        self.sandbox.preflight().ready
    }

    pub(crate) fn hardware_adapter_observations(
        &self,
    ) -> Option<BTreeMap<String, Result<HardwareAdapterObservation, ProviderError>>> {
        self.hardware_adapters
            .as_ref()
            .map(|adapters| adapters.adapter_observations())
    }

    pub(crate) fn refresh_hardware_inventory_observations(
        &self,
        observations: &BTreeMap<String, Result<HardwareAdapterObservation, ProviderError>>,
    ) -> Result<(), ProviderError> {
        let adapters = self.hardware_adapters.as_ref().ok_or_else(|| {
            ProviderError::new(
                "kernel-daemon",
                "HARDWARE_ADAPTERS_NOT_CONFIGURED",
                "per-adapter hardware observations are unavailable",
            )
        })?;
        self.resources
            .refresh_inventory(adapters.aggregate_observations(observations)?)
    }

    /// 获取最新的硬件清单快照
    pub fn inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.inventory_provider.probe_inventory()
    }

    /// Refreshes only current external hardware facts. A successful probe is
    /// committed to the resource ledger; a failed or expired fact leaves the
    /// last allocation state untouched and lets the caller enter DEGRADED.
    /// 仅刷新当前外部硬件事实。探测成功时会提交到资源 ledger；探测失败或事实过期时保留上次分配状态，并由调用方进入 DEGRADED。
    pub fn refresh_inventory_facts(&self) -> Result<InventorySnapshot, ProviderError> {
        let snapshot = self.inventory()?;
        self.resources.refresh_inventory(snapshot.clone())?;
        Ok(snapshot)
    }

    // ════════════════════════════════════════════════════════════════════════
    // 🔧 FUNCTION: KernelDaemon::acquire_lease
    //
    //   Acquires from the last durable inventory ledger only after readiness
    //   has been established; it does not probe hardware inline.
    //
    // 仅在确认资源就绪后从最近一次持久化清单账本中申请租约，不在申请路径内
    // 临时探测硬件，避免事实刷新与分配发生竞态。
    // ════════════════════════════════════════════════════════════════════════
    /// 获取硬件资源租约
    ///
    /// Transport authentication is enforced at the `KernelAuthority` boundary.
    /// The lease holder remains the planned Worker identity, not a
    /// caller-supplied Principal.
    pub fn acquire_lease(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        // Allocation consumes the last durably observed inventory ledger. It
        // must not probe and mutate facts inline, because that would race the
        // caller's snapshot generation between validation and acquisition.
        // The startup/monitor observation paths refresh this ledger separately.
        // 分配使用最近一次已耐久观测到的 inventory ledger。不能在分配过程中同步探测并修改事实，否则验证和获取之间调用方看到的 snapshot generation 会发生竞态。启动和 monitor 观测路径会单独刷新此 ledger。
        let snapshot = self.resources.inventory();
        if !snapshot.capabilities.ready {
            return Err(ProviderError::new(
                "kernel-daemon",
                "ADAPTER_DEGRADED",
                "required hardware adapter capability is not ready",
            ));
        }
        self.resources.acquire_lease(request)
    }

    /// Begins release authority while keeping the physical allocation held.
    /// 开始释放 authority，同时继续占用物理分配资源。
    pub fn begin_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources.begin_release(lease_name, fence_token)
    }

    /// Confirms cleanup and makes a releasing allocation reusable.
    /// 确认清理完成，并使正在释放的分配可重新使用。
    pub fn complete_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources.complete_release(lease_name, fence_token)
    }

    /// Keeps a failed cleanup allocation unavailable.
    /// 清理失败时继续将该分配标记为不可用。
    pub fn fail_release(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources.fail_release(lease_name, fence_token)
    }

    /// Revokes authority without pretending the holder performed a normal
    /// release. Physical resources stay held until `complete_revocation`.
    /// 撤销 authority，但不会假装持有方完成了正常释放。只有 complete_revocation 完成后，物理资源才会解除占用。
    pub fn revoke(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources.revoke(lease_name, fence_token)
    }

    pub fn complete_revocation(
        &self,
        lease_name: &str,
        fence_token: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources.complete_revocation(lease_name, fence_token)
    }

    /// 读取租约当前快照，用于服务层的 fencing 校验与结果回报
    pub fn lease(&self, lease_name: &str) -> Result<ResourceLease, ProviderError> {
        self.resources.get_lease(lease_name)
    }

    /// 列出当前所有跟踪的租约快照，用于权威主动过期与对齐扫描
    pub fn leases(&self) -> Vec<ResourceLease> {
        self.resources.leases()
    }

    /// 检查租约是否仍然持有底层物理资源分配
    pub fn is_allocated(&self, lease_name: &str) -> bool {
        self.resources.is_allocated(lease_name)
    }

    /// Extend a live lease while retaining its exact resource allocation and
    /// fencing authority. The ledger performs the active-state, fence, and
    /// expiry monotonicity checks atomically.
    /// 延长有效租约，同时保留其精确资源分配和 fencing authority。ledger 会原子执行 active 状态、fence 和有效期单调性检查。
    pub fn renew_lease(
        &self,
        lease_name: &str,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources
            .renew_lease(lease_name, fence_token, expires_at_unix_ms)
    }

    /// 为租约内的所有资源合并一份沙箱绑定。
    ///
    /// 当前 SandboxBackend 接口以单份 DeviceBinding 表达资源集合，因此多资源租约在这里合并
    /// 设备节点、环境变量与 GID；任何 Provider 冲突都会 fail closed。
    pub fn binding_for_lease(&self, lease: &ResourceLease) -> Result<DeviceBinding, ProviderError> {
        let snapshot = self.inventory()?;
        let mut bindings = Vec::with_capacity(lease.allocations.len());
        for allocation in &lease.allocations {
            let resource = snapshot
                .resources
                .iter()
                .find(|resource| resource.identity == allocation.resource)
                .ok_or_else(|| {
                    ProviderError::new(
                        "kernel-daemon",
                        "LEASE_RESOURCE_NOT_IN_INVENTORY",
                        &allocation.resource.id,
                    )
                })?;
            bindings.push(
                // Already-acquired lease bindings do not enforce volatile hardware inventory generation checks
                // 已获取的租约 binding 不会强制执行易变的硬件 inventory generation 检查。
                self.resource_provider
                    .create_binding_for_generation(resource, 0)?,
            );
        }
        merge_bindings(bindings)
    }

    /// 根据当前节点底层最新探测事实，组装 Core v1 规范的 [`core_v1::KernelCapabilities`] 能力事实消息。
    ///
    /// # 诚实上报原则
    /// 严格如实上报硬件探测结果，绝不针对缺失的显存、NUMA 或拓扑值进行主观猜测。
    #[allow(deprecated)]
    pub fn get_kernel_capabilities(&self) -> Result<core_v1::KernelCapabilities, ProviderError> {
        let snapshot = self.inventory()?;
        let resources = snapshot
            .resources
            .iter()
            .map(to_semantic_proto_resource)
            .collect();
        let runtime = self.sandbox.preflight();
        Ok(core_v1::KernelCapabilities {
            node: Some(core_v1::NodeRef {
                node_id: self.node_id.clone(),
                node_epoch: self.node_epoch,
            }),
            kernel_version: String::new(),
            inventory_generation: snapshot.generation,
            observed_at: Some(now_timestamp()),
            capacity: None,
            // Deprecated compatibility projection. Kernel no longer decodes
            // vendor or accelerator-specific attributes.
            // 已弃用的兼容性投影。Kernel 不再解码厂商或加速器专属属性。
            accelerators: Vec::new(),
            sandbox_backends: vec![self.sandbox.backend_id().to_string()],
            enforcement: runtime
                .enforcement
                .into_iter()
                .map(|report| core_v1::EnforcementReport {
                    // A process-tree cgroup report does not prove GPU device isolation.
                    // 进程树监管不能作为 GPU 设备硬隔离证据。
                    resource_kind: match report.resource_kind.as_str() {
                        "cpu" | "compute.cpu" => core_v1::ResourceKind::Cpu,
                        "memory" | "memory.ram" => core_v1::ResourceKind::Memory,
                        "accelerator" => core_v1::ResourceKind::Accelerator,
                        _ => core_v1::ResourceKind::Unspecified,
                    } as i32,
                    mode: to_proto_enforcement(report.mode) as i32,
                    adapter_id: report.adapter_id,
                    reason_code: report.reason_code,
                })
                .collect(),
            feature_flags: snapshot
                .capabilities
                .facts
                .into_iter()
                .filter(|fact| fact.available)
                .map(|fact| fact.name)
                .collect(),
            resources,
        })
    }
}
