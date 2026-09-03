// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/inventory.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 硬件设备与清单快照模型.

use std::path::PathBuf;

use cy_kernel_contract as semantic;

use crate::capability::NodeCapabilities;

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

/// 节点硬件清单快照 (Inventory Snapshot)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventorySnapshot {
    /// 状态版本世代代数（每次硬件或拓扑变动单调递增）
    pub generation: u64,
    /// Provider 发布的通用资源事实。Kernel 只解释 Semantic Contract 的
    /// class/capability/capacity 匹配规则，不解释厂商属性。
    pub resources: Vec<semantic::Resource>,
    /// 当前节点的综合能力与隔离策略
    pub capabilities: NodeCapabilities,
}
