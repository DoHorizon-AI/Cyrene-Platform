//! CYRENE 节点内核守护进程组合根 (Kernel Daemon Composition Root).
//!
//! 【内核守护进程职责与设计哲学】
//! 本模块作为节点内核守护进程的装配中心（Composition Root），负责将硬件探测、资源租约与沙箱运行时等各个六边形端口组合连接：
//! 1. **事实汇聚与映射**：聚合底层适配器上报的不可变硬件事实，将其严格、诚实地映射为 Core v1 Protobuf 消息（[`core_v1::KernelCapabilities`]）；
//! 2. **绝不猜测（No Guessing Invariant）**：对于探测不到的显存容量、NUMA 节点或拓扑链路，严格上报未知，绝不用启发式猜测伪造数据；
//! 3. **职责边界**：内核守护进程专职负责单机节点物理事实与资源隔离，不包含远程制品下载、全局调度仲裁或跨重启接管僵尸进程的逻辑。

use std::sync::Arc;

use cy_hardware_discovery::NvidiaSmiProvider;
use cy_kernel_api::{
    AcceleratorKind, AcceleratorLinkType, AcceleratorProvider, AcceleratorVendor,
    HostInventoryProvider, InventorySnapshot, ProviderError, ResourceLease, ResourceLeaseManager,
    ResourceRequest, SandboxBackend,
};
use cy_proto::core_v1;

/// 节点内核守护进程核心结构体
pub struct KernelDaemon {
    /// 宿主机硬件清单提供者
    inventory_provider: Arc<dyn HostInventoryProvider>,
    /// 硬件加速卡提供者
    accelerator_provider: Arc<dyn AcceleratorProvider>,
    /// 硬件资源租约管理器
    resources: Arc<dyn ResourceLeaseManager>,
    /// 沙箱隔离后端
    sandbox: Arc<dyn SandboxBackend>,
    /// 节点唯一 ID
    node_id: String,
    /// 节点启动纪元代数 (Epoch)
    node_epoch: u64,
}

impl KernelDaemon {
    /// 构造新的内核守护进程实例
    pub fn new(
        inventory_provider: Arc<dyn HostInventoryProvider>,
        accelerator_provider: Arc<dyn AcceleratorProvider>,
        resources: Arc<dyn ResourceLeaseManager>,
        sandbox: Arc<dyn SandboxBackend>,
        node_id: impl Into<String>,
        node_epoch: u64,
    ) -> Self {
        Self {
            inventory_provider,
            accelerator_provider,
            resources,
            sandbox,
            node_id: node_id.into(),
            node_epoch,
        }
    }

    /// 检查节点基础沙箱环境是否已就绪
    pub fn preflight_ready(&self) -> bool {
        self.sandbox.preflight().ready
    }

    /// 获取最新的硬件清单快照
    pub fn inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        self.inventory_provider.probe_inventory()
    }

    /// 申请预留硬件资源租约
    pub fn reserve(&self, request: ResourceRequest) -> Result<ResourceLease, ProviderError> {
        self.resources.reserve(request)
    }

    /// 释放硬件资源租约
    pub fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError> {
        self.resources.release(lease_name, fence_token)
    }

    /// 根据当前节点底层最新探测事实，组装 Core v1 规范的 [`core_v1::KernelCapabilities`] 能力事实消息。
    ///
    /// # 诚实上报原则
    /// 严格如实上报硬件探测结果，绝不针对缺失的显存、NUMA 或拓扑值进行主观猜测。
    pub fn get_kernel_capabilities(&self) -> Result<core_v1::KernelCapabilities, ProviderError> {
        let snapshot = self.inventory()?;
        let accelerators = snapshot
            .devices
            .iter()
            .map(|device| core_v1::AcceleratorDevice {
                device_id: device.device_id.clone(),
                kind: to_proto_kind(device.kind) as i32,
                vendor: to_proto_vendor(device.vendor) as i32,
                other_vendor_id: String::new(),
                device_family: device.device_family.clone(),
                pci_address: device.pci_address.clone().unwrap_or_default(),
                total_memory_bytes: device.total_memory_bytes.unwrap_or_default(),
                allocatable_memory_bytes: device.allocatable_memory_bytes.unwrap_or_default(),
                features: device.features.clone(),
                partitions: Vec::new(),
                health: Some(core_v1::HealthReport {
                    status: device.health.healthy.map_or(
                        core_v1::HealthStatus::Unknown as i32,
                        |healthy| {
                            if healthy {
                                core_v1::HealthStatus::Healthy as i32
                            } else {
                                core_v1::HealthStatus::Degraded as i32
                            }
                        },
                    ),
                    reason_code: device.health.reason_code.clone(),
                    summary: device.health.summary.clone(),
                }),
                numa_node: device.numa_node,
                links: device
                    .links
                    .iter()
                    .map(|link| core_v1::AcceleratorLink {
                        peer_device_id: link.peer_device_id.clone(),
                        link_type: to_proto_link_type(link.link_type) as i32,
                        link_count: link.link_count.unwrap_or_default(),
                        width: link.width.unwrap_or_default(),
                        bandwidth_bytes_per_second: link
                            .bandwidth_bytes_per_second
                            .unwrap_or_default(),
                        stable: link.stable,
                    })
                    .collect(),
            })
            .collect();
        let runtime = self.sandbox.preflight();
        Ok(core_v1::KernelCapabilities {
            node: Some(core_v1::NodeRef {
                node_id: self.node_id.clone(),
                node_epoch: self.node_epoch,
            }),
            kernel_version: String::new(),
            inventory_generation: snapshot.generation,
            observed_at: Some(prost_types::Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            capacity: None,
            accelerators,
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
        })
    }

    /// 获取内置 NVIDIA 适配器引用
    pub fn nvidia_provider(&self) -> &Arc<dyn AcceleratorProvider> {
        &self.accelerator_provider
    }
}

/// 将内部芯片类型转换为 Protobuf 协议枚举
fn to_proto_kind(kind: AcceleratorKind) -> core_v1::AcceleratorKind {
    match kind {
        AcceleratorKind::Gpu => core_v1::AcceleratorKind::Gpu,
        AcceleratorKind::Npu => core_v1::AcceleratorKind::Npu,
        AcceleratorKind::Tpu => core_v1::AcceleratorKind::Tpu,
        AcceleratorKind::Other => core_v1::AcceleratorKind::Other,
    }
}

/// 将内部厂商类型转换为 Protobuf 协议枚举
fn to_proto_vendor(vendor: AcceleratorVendor) -> core_v1::AcceleratorVendor {
    match vendor {
        AcceleratorVendor::Nvidia => core_v1::AcceleratorVendor::Nvidia,
        AcceleratorVendor::Amd => core_v1::AcceleratorVendor::Amd,
        AcceleratorVendor::HuaweiAscend => core_v1::AcceleratorVendor::HuaweiAscend,
        AcceleratorVendor::Intel => core_v1::AcceleratorVendor::Intel,
        AcceleratorVendor::Other => core_v1::AcceleratorVendor::Other,
    }
}

/// 将内部互联总线类型转换为 Protobuf 协议枚举
fn to_proto_link_type(link_type: AcceleratorLinkType) -> core_v1::AcceleratorLinkType {
    match link_type {
        AcceleratorLinkType::Pcie => core_v1::AcceleratorLinkType::Pcie,
        AcceleratorLinkType::Nvlink => core_v1::AcceleratorLinkType::Nvlink,
        AcceleratorLinkType::Xgmi => core_v1::AcceleratorLinkType::Xgmi,
        AcceleratorLinkType::Other => core_v1::AcceleratorLinkType::Other,
    }
}

/// 将内部隔离执行模式转换为 Protobuf 协议枚举
fn to_proto_enforcement(mode: cy_kernel_api::EnforcementMode) -> core_v1::EnforcementMode {
    match mode {
        cy_kernel_api::EnforcementMode::Hard => core_v1::EnforcementMode::Hard,
        cy_kernel_api::EnforcementMode::Soft => core_v1::EnforcementMode::Soft,
        cy_kernel_api::EnforcementMode::VisibilityOnly => core_v1::EnforcementMode::VisibilityOnly,
        cy_kernel_api::EnforcementMode::ObserveOnly => core_v1::EnforcementMode::ObserveOnly,
        cy_kernel_api::EnforcementMode::Unenforced => core_v1::EnforcementMode::Unenforced,
    }
}

#[allow(dead_code)]
fn _default_nvidia_provider() -> Arc<dyn AcceleratorProvider> {
    Arc::new(NvidiaSmiProvider::new("nvidia-smi"))
}
