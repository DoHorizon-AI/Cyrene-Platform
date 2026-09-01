// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-api/src/capability.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 资源隔离与节点能力事实模型.

/// 资源隔离与限制的强制执行模式 (Enforcement Mode)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementMode {
    /// 强隔离（硬件级 / 内核 cgroup 级强制约束）
    Hard,
    /// 软隔离（进程级配额警告）
    Soft,
    /// 仅通过 Provider 管理的可见性环境变量隔离
    VisibilityOnly,
    /// 仅观察监控，不做任何拦截
    ObserveOnly,
    /// 未启用任何隔离措施
    Unenforced,
}

/// 资源隔离执行情况报告
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementReport {
    /// 资源类型（如 "accelerator", "memory", "cpu"）
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
    /// 能力名称（如 "cgroup_v2", "resource_binding"）
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
