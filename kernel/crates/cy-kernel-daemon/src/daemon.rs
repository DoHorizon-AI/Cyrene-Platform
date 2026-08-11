//! 节点内核守护进程核心结构与组合根实现。

use std::sync::Arc;

use cy_adapter_client::{HardwareAdapterEndpoint, UdsHardwareAdapterRegistry};
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
            resources,
            sandbox,
            node_id: node_id.into(),
            node_epoch,
        }
    }

    /// 通过版本化 UDS 连接进程外硬件适配器注册表。
    ///
    /// 该组合根不加载厂商动态库，也不执行任何厂商探测命令。适配器失联时，端口
    /// 返回 `ADAPTER_UNAVAILABLE`，由上层将节点转为不可继续分配的降级状态。
    pub fn with_hardware_adapters(
        endpoints: impl IntoIterator<Item = HardwareAdapterEndpoint>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Result<Self, ProviderError> {
        let adapters = Arc::new(UdsHardwareAdapterRegistry::from_endpoints(endpoints)?);
        Ok(Self::new(
            adapters.clone(),
            adapters,
            resources,
            sandbox,
            node_id,
            node_epoch,
        ))
    }

    /// 检查节点基础沙箱环境是否已就绪
    pub fn preflight_ready(&self) -> bool {
        self.sandbox.preflight().ready
            && self
                .inventory()
                .is_ok_and(|snapshot| snapshot.capabilities.ready)
    }

    /// 获取最新的硬件清单快照
    pub fn inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.inventory_provider.probe_inventory()
    }

    /// Refreshes only current external hardware facts. A successful probe is
    /// committed to the resource ledger; a failed or expired fact leaves the
    /// last allocation state untouched and lets the caller enter DEGRADED.
    pub fn refresh_inventory_facts(&self) -> Result<InventorySnapshot, ProviderError> {
        let snapshot = self.inventory()?;
        self.resources.refresh_inventory(snapshot.clone())?;
        Ok(snapshot)
    }

    /// 申请预留硬件资源租约
    pub fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        let snapshot = self.inventory()?;
        if !snapshot.capabilities.ready {
            return Err(ProviderError::new(
                "kernel-daemon",
                "ADAPTER_DEGRADED",
                "required hardware adapter capability is not ready",
            ));
        }
        self.resources.refresh_inventory(snapshot)?;
        self.resources.reserve(request)
    }

    /// 释放硬件资源租约
    pub fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError> {
        self.resources.release(lease_name, fence_token)
    }

    /// 读取租约当前快照，用于服务层的 fencing 校验与结果回报
    pub fn lease(&self, lease_name: &str) -> Result<ResourceLease, ProviderError> {
        self.resources.get_lease(lease_name)
    }

    /// Extend a live lease while retaining its exact resource allocation and
    /// fencing authority. The ledger performs the active-state, fence, and
    /// expiry monotonicity checks atomically.
    pub fn renew(
        &self,
        lease_name: &str,
        fence_token: u64,
        expires_at_unix_ms: u64,
    ) -> Result<ResourceLease, ProviderError> {
        self.resources
            .renew(lease_name, fence_token, expires_at_unix_ms)
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
                self.resource_provider
                    .create_binding_for_generation(resource, lease.inventory_generation)?,
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
            accelerators: Vec::new(),
            sandbox_backends: vec![self.sandbox.backend_id().to_string()],
            enforcement: runtime
                .enforcement
                .into_iter()
                .map(|report| core_v1::EnforcementReport {
                    resource_kind: core_v1::ResourceKind::Accelerator as i32,
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
