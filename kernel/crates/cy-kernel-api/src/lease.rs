// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/lease.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 资源租约、配额上限与分配模型.

use std::collections::BTreeMap;

use cy_kernel_contract as semantic;

use crate::capability::EnforcementMode;

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

/// 资源租约生命周期状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    /// 处于活跃使用中
    Active,
    /// 正在释放回收中
    Releasing,
    /// 已彻底释放
    Released,
    /// TTL 已到期并由账本收回。
    Expired,
    /// 权威主动撤销。
    Revoked,
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
    /// 受租约保护的 Worker/holder 身份。
    pub holder: semantic::Identity,
    /// 由控制面编译后的有界、厂商无关资源查询。
    pub query: semantic::ResourceQuery,
    /// 租约绝对过期时间（Unix 毫秒）。
    pub expires_at_unix_ms: Option<u64>,
    /// 进程 cgroup 应执行的 CPU、内存与 CPU 集合限制。
    pub limits: CgroupLimits,
}

/// 单项设备资源分配结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAllocation {
    /// 分配 ID
    pub allocation_id: String,
    /// 实际分配的资源身份与代次。
    pub resource: semantic::Identity,
    /// 分配时批准的通用容量快照。
    pub granted_capacity: BTreeMap<String, semantic::Quantity>,
    /// 生效的隔离模式
    pub enforcement: EnforcementMode,
}

/// 资源租约凭证 (Resource Lease)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLease {
    /// 租约名称
    pub name: String,
    /// 租约身份代次；同名租约重建时必须递增。
    pub generation: u64,
    /// 当前状态
    pub state: LeaseState,
    /// 当前资源所有权持有者。
    pub holder: semantic::Identity,
    /// 具体的设备分配清单
    pub allocations: Vec<ResourceAllocation>,
    /// 创建租约时的硬件世代
    pub inventory_generation: u64,
    /// 隔离围栏令牌（Fence Token：递增的单调计数器，防止旧任务迟到的写操作污染新租约）
    pub fence_token: u64,
    /// 租约绝对过期时间（Unix 毫秒）。
    pub expires_at_unix_ms: Option<u64>,
    /// 与该租约绑定、启动时必须写入 cgroup 的资源上限。
    pub limits: CgroupLimits,
}
