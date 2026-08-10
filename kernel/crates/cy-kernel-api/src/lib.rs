//! CYRENE 节点内核核心 API 契约与端口定义 (Kernel Core Ports & Fact Types).
//!
//! 【六边形架构与端口-适配器模式 (Hexagonal Architecture)】
//! 本 Crate 作为节点内核的核心抽象契约层，**刻意不包含任何特定 Linux 系统调用、特定 GPU 厂商 SDK、gRPC (Tonic) 或底层进程实现的细节**。
//! 通用端口的实现（如进程外硬件适配器客户端、cgroup v2 沙箱、内存租约管理器）向内核上报事实（Facts），
//! 内核守护进程组合根将其组装暴露为统一的 Core v1 平台服务：
//!
//! - **硬件资产与事实**：[`AcceleratorDevice`], [`DeviceNode`], [`AcceleratorLink`], [`HealthReport`]
//! - **资源租约与围栏**：[`ResourceLease`], [`ResourceRequest`], [`DeviceBinding`], [`EnforcementMode`]
//! - **沙箱与进程生命周期**：[`LaunchPlan`], [`ProcessHandle`], [`CleanupReport`], [`StopRequest`]
//! - **核心端口 Trait**：[`AcceleratorProvider`], [`ResourceLeaseManager`], [`ProcessRuntime`], [`SandboxBackend`] 等

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf, time::Duration};

/// 资源隔离与限制的强制执行模式 (Enforcement Mode)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementMode {
    /// 强隔离（硬件级 / 内核 cgroup 级强制约束）
    Hard,
    /// 软隔离（进程级配额警告）
    Soft,
    /// 仅通过可见性环境变量隔离（如 `CUDA_VISIBLE_DEVICES`）
    VisibilityOnly,
    /// 仅观察监控，不做任何拦截
    ObserveOnly,
    /// 未启用任何隔离措施
    Unenforced,
}

/// 资源隔离执行情况报告
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementReport {
    /// 资源类型（如 "gpu", "memory", "cpu"）
    pub resource_kind: String,
    /// 实际生效的强制模式
    pub mode: EnforcementMode,
    /// 执行该策略的适配器 ID
    pub adapter_id: String,
    /// 决策或执行原因代码
    pub reason_code: String,
}

/// 节点单项能力事实项 (Capability Fact)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFact {
    /// 能力名称（如 "cgroup_v2", "nvidia_driver", "pcie_p2p"）
    pub name: String,
    /// 当前是否可用
    pub available: bool,
    /// 是否为调度必需项
    pub required: bool,
    /// 详细说明或诊断信息
    pub detail: String,
}

/// 节点综合能力事实集合 (Node Capabilities)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCapabilities {
    /// 节点是否处于就绪可用状态
    pub ready: bool,
    /// 节点各项能力事实列表
    pub facts: Vec<CapabilityFact>,
    /// 当前生效的资源隔离策略报告
    pub enforcement: Vec<EnforcementReport>,
}

/// 硬件加速芯片种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceleratorKind {
    /// 图形处理器 (GPU)
    Gpu,
    /// 神经网络处理器 (NPU)
    Npu,
    /// 张量处理器 (TPU)
    Tpu,
    /// 其他专用加速芯片
    Other,
}

/// 硬件加速芯片厂商分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceleratorVendor {
    /// 英伟达 (NVIDIA)
    Nvidia,
    /// 超威半导体 (AMD ROCm)
    Amd,
    /// 华为昇腾 (Huawei Ascend)
    HuaweiAscend,
    /// 英特尔 (Intel Gaudi / Xe)
    Intel,
    /// 其他厂商
    Other,
}

/// 芯片间高速互联总线类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceleratorLinkType {
    /// PCIe 总线
    Pcie,
    /// NVIDIA NVLink 互联
    Nvlink,
    /// AMD xGMI / Infinity Fabric
    Xgmi,
    /// 其他专用高速互联
    Other,
}

/// 加速卡间互联拓扑链路 (Accelerator Link)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceleratorLink {
    /// 对端对等设备 ID
    pub peer_device_id: String,
    /// 互联链路类型
    pub link_type: AcceleratorLinkType,
    /// 链路通道数量（如 4x NVLink）
    pub link_count: Option<u32>,
    /// 链路位宽
    pub width: Option<u32>,
    /// 理论带宽（字节/秒）
    pub bandwidth_bytes_per_second: Option<u64>,
    /// 拓扑连接状态是否稳定
    pub stable: bool,
}

/// 硬件设备健康状况报告
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// 是否健康（`Some(true)` 正常，`Some(false)` 故障，`None` 未知）
    pub healthy: Option<bool>,
    /// 诊断原因代码
    pub reason_code: String,
    /// 状态摘要描述
    pub summary: String,
}

/// 操作系统设备字符/块设备节点（路径由进程外硬件适配器返回）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNode {
    /// 设备文件路径
    pub path: PathBuf,
    /// 主设备号 (Major Device Number)
    pub major: Option<u32>,
    /// 次设备号 (Minor Device Number)
    pub minor: Option<u32>,
    /// 运行该设备是否必须挂载
    pub required: bool,
}

/// 加速卡物理设备完整事实模型 (Accelerator Device)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceleratorDevice {
    /// 设备全局唯一标识（例如："gpu-0000:01:00.0"）
    pub device_id: String,
    /// 产生此不可变硬件事实的外部 Adapter 标识。
    ///
    /// 这不是厂商 API，而是 Kernel 用于把设备绑定请求路由回同一 UDS
    /// Sidecar 的来源证明。资源账本不能根据 `vendor` 猜测路由。
    pub adapter_id: String,
    /// 设备类型（GPU/NPU 等）
    pub kind: AcceleratorKind,
    /// 硬件厂商
    pub vendor: AcceleratorVendor,
    /// 架构/产品家族（如 "Ada Lovelace", "Hopper", "CDNA3"）
    pub device_family: String,
    /// PCI 总线地址（例如："0000:01:00.0"）
    pub pci_address: Option<String>,
    /// 绑定的 NUMA 内存节点编号
    pub numa_node: Option<i32>,
    /// 物理显存总量（字节数）
    pub total_memory_bytes: Option<u64>,
    /// 允许分配给工作负载的最大显存量（字节数）
    pub allocatable_memory_bytes: Option<u64>,
    /// 硬件支持的特性列表（如 `["tensor_cores", "fp8", "flash_attention"]`）
    pub features: Vec<String>,
    /// 需要映射进容器/沙箱的系统设备节点列表
    pub device_nodes: Vec<DeviceNode>,
    /// 与其他加速卡的互联拓扑链路
    pub links: Vec<AcceleratorLink>,
    /// 设备健康状态
    pub health: HealthReport,
}

/// 节点硬件清单快照 (Inventory Snapshot)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventorySnapshot {
    /// 状态版本世代代数（每次硬件或拓扑变动单调递增）
    pub generation: u64,
    /// 当前探测到的所有加速卡设备
    pub devices: Vec<AcceleratorDevice>,
    /// 当前节点的综合能力与隔离策略
    pub capabilities: NodeCapabilities,
}

/// 设备绑定配置 (Device Binding)：定义特定沙箱进程对硬件设备的访问授权与环境变量
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinding {
    /// 绑定的物理设备 ID
    pub device_id: String,
    /// 需要注入沙箱的设备节点
    pub nodes: Vec<DeviceNode>,
    /// Adapter 返回、由 Kernel 受控注入的设备环境变量
    pub environment: BTreeMap<String, String>,
    /// 访问该设备必需的系统用户组 GID 列表
    pub required_gids: Vec<u32>,
    /// 强制执行模式
    pub enforcement: EnforcementMode,
    /// 执行绑定的适配器标识
    pub adapter_id: String,
    /// 绑定决策原因代码
    pub reason_code: String,
}

impl DeviceBinding {
    /// 合并用户自定义环境变量与内核设备绑定环境变量。
    ///
    /// # 安全保护（核心约束）
    /// 严格禁止插件/用户代码覆盖 Adapter 已为设备绑定声明的环境变量。
    /// 一旦发现冲突立即报错，防止越权访问未分配的 GPU 设备。
    pub fn merge_environment(
        &self,
        requested: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        if requested
            .keys()
            .any(|key| self.environment.contains_key(key))
        {
            return Err(ProviderError::new(
                &self.adapter_id,
                "RESERVED_ENVIRONMENT",
                "plugin attempted to override an Adapter-owned device variable",
            ));
        }

        let mut environment = requested.clone();
        environment.extend(self.environment.clone());
        Ok(environment)
    }
}

/// 资源租约生命周期状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    /// 处于活跃使用中
    Active,
    /// 正在释放回收中
    Releasing,
    /// 已彻底释放
    Released,
    /// 分配或执行失败
    Failed,
    /// 出现硬件故障已被隔离封锁
    Quarantined,
}

/// 资源预留请求参数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRequest {
    /// 租约唯一名称
    pub lease_name: String,
    /// 客户端发起请求时所基于的硬件清单世代代数（用于乐观并发控制）
    pub expected_inventory_generation: u64,
    /// 请求分配的加速卡数量
    pub count: usize,
    /// 指定厂商要求（可选）
    pub vendor: Option<AcceleratorVendor>,
    /// 单卡最低显存要求（字节数，可选）
    pub min_memory_bytes: Option<u64>,
    /// 进程 cgroup 应执行的 CPU、内存与 CPU 集合限制。
    pub limits: CgroupLimits,
}

/// 由 Kernel 执行的 cgroup v2 配额。
///
/// 这些字段是纯数值契约；具体的 `cpu.max`、`memory.max` 与 `cpuset.cpus`
/// 写入属于 Linux Sandbox 适配器，避免资源账本依赖宿主机实现细节。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupLimits {
    /// CPU 上限，单位为 millicores；`None` 表示不设置 `cpu.max`。
    pub cpu_max_millicores: Option<u32>,
    /// 内存上限，单位为字节；`None` 表示不设置 `memory.max`。
    pub memory_max_bytes: Option<u64>,
    /// 可运行 CPU 集合，例如 `"0-3,8-11"`；`None` 表示不设置 `cpuset.cpus`。
    pub cpuset_cpus: Option<String>,
}

/// 单项设备资源分配结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAllocation {
    /// 分配 ID
    pub allocation_id: String,
    /// 实际分配的设备 ID
    pub device_id: String,
    /// 批准分配的显存额度
    pub granted_memory_bytes: Option<u64>,
    /// 生效的隔离模式
    pub enforcement: EnforcementMode,
}

/// 资源租约凭证 (Resource Lease)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLease {
    /// 租约名称
    pub name: String,
    /// 当前状态
    pub state: LeaseState,
    /// 具体的设备分配清单
    pub allocations: Vec<ResourceAllocation>,
    /// 创建租约时的硬件世代
    pub inventory_generation: u64,
    /// 隔离围栏令牌（Fence Token：递增的单调计数器，防止旧任务迟到的写操作污染新租约）
    pub fence_token: u64,
    /// 与该租约绑定、启动时必须写入 cgroup 的资源上限。
    pub limits: CgroupLimits,
}

/// 进程沙箱启动计划 (Launch Plan)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// 进程实例名称
    pub instance_name: String,
    /// 可执行程序文件路径
    pub executable: PathBuf,
    /// 启动命令行参数
    pub args: Vec<String>,
    /// 环境变量键值对
    pub environment: BTreeMap<String, String>,
    /// 分配给该进程的 cgroup 作用域组名
    pub cgroup_name: String,
    /// 进程启动前必须生效的 cgroup 配额。
    pub limits: CgroupLimits,
}

/// Immutable identity of an installation that has already passed installer
/// verification. It deliberately contains no product/plugin implementation
/// details, executable path, arguments, or environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInstallation {
    pub installation_name: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
}

/// Port for obtaining a launch plan from a previously verified installation.
///
/// Install layout and record parsing belong to an outer adapter. Before the
/// result is launched, the Kernel still overlays lease limits, reserved
/// heartbeat values, and device-binding environment restrictions.
pub trait InstalledPluginResolver: Send + Sync {
    fn resolve_launch_plan(
        &self,
        installation: &VerifiedInstallation,
        instance_name: &str,
    ) -> Result<LaunchPlan, ProviderError>;
}

/// 由 cgroup v2 直接读取的真实物理消耗量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupTelemetry {
    /// `memory.current`，单位为字节。
    pub memory_current_bytes: Option<u64>,
    /// `memory.peak`，单位为字节；旧内核不可用时为 `None`。
    pub memory_peak_bytes: Option<u64>,
    /// `cpu.stat` 的总使用时间，单位为微秒。
    pub cpu_usage_usec: Option<u64>,
    /// `cpu.stat` 的用户态使用时间，单位为微秒。
    pub cpu_user_usec: Option<u64>,
    /// `cpu.stat` 的内核态使用时间，单位为微秒。
    pub cpu_system_usec: Option<u64>,
    /// `memory.events.local` 的 `oom_kill` 计数。
    pub oom_kill_count: u64,
}

/// 已启动沙箱进程的句柄引用 (Process Handle)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessHandle {
    /// 主进程操作系统 PID
    pub pid: u32,
    /// 对应的 Linux cgroup v2 路径
    pub cgroup_path: PathBuf,
    /// 进程启动时间滴答数（用于校验 PID 复用）
    pub start_time_ticks: Option<u64>,
}

/// 进程运行状态与异常条件
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCondition {
    /// 状态原因代码
    pub reason_code: String,
    /// 状态描述摘要
    pub summary: String,
}

/// 进程退出与资源清理报告 (Cleanup Report)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    /// 清理操作是否彻底完成（进程树已全部清空）
    pub complete: bool,
    /// 进程最终退出码
    pub exit_code: Option<i32>,
    /// 是否被 Linux OOM Killer 强行终止
    pub oom_killed: bool,
    /// 退出期间发生的异常条件列表
    pub conditions: Vec<ProcessCondition>,
    /// 退出原因代码
    pub reason_code: String,
}

/// 进程停止请求参数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRequest {
    /// 优雅停机宽限期（超时后升级为 SIGKILL 强杀）
    pub grace_period: Duration,
    /// 是否跳过优雅停机直接立即强杀
    pub immediate: bool,
}

/// 内核适配器通用错误模型
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    /// 抛出错误的适配器 ID
    pub adapter_id: String,
    /// 机器可读的原因代码
    pub reason_code: String,
    /// 人类可读的错误详情
    pub message: String,
}

impl ProviderError {
    /// 构造新的适配器错误实例
    pub fn new(adapter_id: &str, reason_code: &str, message: &str) -> Self {
        Self {
            adapter_id: adapter_id.to_string(),
            reason_code: reason_code.to_string(),
            message: message.to_string(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason_code, self.message)
    }
}

impl Error for ProviderError {}

/// 端口 Trait 1：宿主机全量硬件清单探测器
pub trait HostInventoryProvider: Send + Sync {
    /// 采集并返回节点硬件清单快照
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError>;
}

/// 端口 Trait 2：特定厂商硬件加速卡适配器
pub trait AcceleratorProvider: Send + Sync {
    /// 适配器唯一 ID（例如："hardware-adapter-uds"）
    fn adapter_id(&self) -> &str;
    /// 探测该厂商下的所有加速设备
    fn probe_inventory(&self) -> Result<Vec<AcceleratorDevice>, ProviderError>;
    /// 为指定设备创建安全隔离绑定规则
    fn create_binding(&self, device: &AcceleratorDevice) -> Result<DeviceBinding, ProviderError>;
    /// 为指定库存代次创建绑定。
    ///
    /// 旧的或纯静态实现可以安全地沿用无代次的默认实现；进程外 Adapter Host
    /// 必须覆盖此方法并将代次传给其协议端点。
    fn create_binding_for_generation(
        &self,
        device: &AcceleratorDevice,
        _expected_inventory_generation: u64,
    ) -> Result<DeviceBinding, ProviderError> {
        self.create_binding(device)
    }
    /// 读取指定设备的健康状态
    fn read_health(&self, device_id: &str) -> Result<HealthReport, ProviderError>;
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
    /// 凭围栏令牌释放已占用的资源租约
    fn release(&self, lease_name: &str, fence_token: u64) -> Result<(), ProviderError>;
}

/// A compact lifecycle record that the pure Kernel can emit to an outer
/// durable journal. The record deliberately carries only node identity,
/// fencing and terminal lifecycle facts; it never serializes launch commands,
/// environment variables, driver data, or Worker payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJournalRecord {
    pub event: RuntimeJournalEvent,
    pub node_id: String,
    pub node_epoch: u64,
    pub instance_name: Option<String>,
    pub lease_name: Option<String>,
    pub fence_token: Option<u64>,
    pub reason_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeJournalEvent {
    KernelStarted,
    LeaseReserved,
    LeaseReleased,
    InstanceLaunched,
    InstanceTerminated,
    InstanceCleanupFailed,
    WatchdogReaped,
}

/// Outer runtime persistence port. Implementations belong in `runtime/` or an
/// external service; Kernel decision code never opens journal files itself.
pub trait RuntimeJournalSink: Send + Sync {
    fn append(&self, record: RuntimeJournalRecord) -> Result<(), ProviderError>;
}

#[derive(Debug, Default)]
pub struct NoopRuntimeJournal;

impl RuntimeJournalSink for NoopRuntimeJournal {
    fn append(&self, _record: RuntimeJournalRecord) -> Result<(), ProviderError> {
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_binding_environment_is_reserved_without_vendor_knowledge() {
        let binding = DeviceBinding {
            device_id: "accelerator-1".to_string(),
            nodes: Vec::new(),
            environment: BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "1".to_string())]),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::VisibilityOnly,
            adapter_id: "test-adapter".to_string(),
            reason_code: "TEST".to_string(),
        };
        let requested =
            BTreeMap::from([("ADAPTER_VISIBLE_DEVICE".to_string(), "other".to_string())]);
        assert_eq!(
            binding
                .merge_environment(&requested)
                .unwrap_err()
                .reason_code,
            "RESERVED_ENVIRONMENT"
        );
    }
}
