// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/training.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 训练、工作负载、决策报告与检查点契约数据结构定义。
//!
//! 包含工作负载请求（WorkloadRequest）、决策原因报告（WhyReport）、
//! 环境校验结果（ValidationResult）、训练修订版本（TrainingRevision）及检查点元数据（CheckpointMetadata）。
//!
//! `MIGRATING_COMPATIBILITY`: Product-owned training lifecycle types remain
//! here only for the implemented v0 SPI and canonical-hash compatibility.
//! Yield owns new TrainingRun, revision, and checkpoint business state. Do not
//! extend these types for new Product behavior.

use serde::{Deserialize, Serialize};

use super::runtime::ValidationLevel;
use super::ObjectSnapshot;

// ---------------------------------------------------------------------------
// WorkloadRequest（工作负载请求）
// ---------------------------------------------------------------------------

/// 任务负载类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Workload {
    /// 模型微调 / 训练任务
    Finetune,
    /// 在线推理 / API 服务任务
    Serve,
}

/// 优化目标优先级策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// 极致低延迟优先（TTFT 与每 Token 生成延迟最低）
    Latency,
    /// 极致高吞吐优先（单位时间处理更多 Batch 请求）
    Throughput,
    /// 成本节约优先（尽量使用低成本或少卡资源）
    Cost,
    /// 生成质量优先（尽量采用高精度，不使用重度量化）
    Quality,
}

/// 调度与规划的目标设定
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Objective {
    /// 优先级
    pub priority: Priority,
}

/// 任务运行的资源与成本约束
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constraints {
    /// 允许分配的最大 GPU 卡数
    pub max_gpu_count: u32,
    /// 允许的最大预算/成本上限（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_cost: Option<f64>,
}

/// 工作负载请求：用户或上层控制面发起的任务调度诉求
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadRequest {
    /// 目标模型名称或路径（例如："meta-llama/Llama-3-8B"）
    pub model: String,
    /// 工作负载类型（微调或推理）
    pub workload: Workload,
    /// 训练数据集标识符或路径（仅在微调任务中有效，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset: Option<String>,
    /// 优化目标
    pub objective: Objective,
    /// 硬性约束条件
    pub constraints: Constraints,
}

// ---------------------------------------------------------------------------
// WhyReport（决策原因报告）
// ---------------------------------------------------------------------------

/// 决策判定结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// 选中/采纳
    Chosen,
    /// 否决/拒绝
    Rejected,
}

/// 决策置信度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// 低置信度
    Low,
    /// 中置信度
    Medium,
    /// 高置信度
    High,
}

/// 单项决策详情记录
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// 决策主题（例如："gpu_selection", "quantization_strategy"）
    pub subject: String,
    /// 判定结论（选中或拒绝）
    pub verdict: Verdict,
    /// 决策原由说明
    pub rationale: String,
    /// 支撑该决策的证据列表（例如测试指标、报错日志或兼容性规则）
    pub evidence: Vec<String>,
    /// 决策置信度
    pub confidence: Confidence,
    /// 该决策背后的证据等级（来自验证阶梯）
    pub evidence_level: ValidationLevel,
}

/// 决策原因报告（WhyReport）：记录调度器或专家规则为何选择/拒绝某些组合的可解释性报告
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhyReport {
    /// 决策条目列表
    pub decisions: Vec<Decision>,
    /// 总体总结（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// ValidationResult（校验结果）
// ---------------------------------------------------------------------------

/// 单项检查状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// 通过
    Pass,
    /// 失败
    Fail,
    /// 跳过
    Skip,
}

/// 单项校验检查明细
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// 检查项名称（例如："cuda_driver_compat", "vram_sufficient"）
    pub name: String,
    /// 检查状态
    pub status: CheckStatus,
    /// 检查输出证据/详情（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub evidence: Option<String>,
}

/// 运行时环境验证结果汇总
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    /// 所验证的 RuntimeManifest 标识符 ("sha256:<hex>")
    pub runtime_id: String,
    /// 当前所达到的验证等级
    pub level: ValidationLevel,
    /// 具体检查项列表
    pub checks: Vec<Check>,
    /// 支撑证据的等级
    pub evidence_level: ValidationLevel,
}

// ---------------------------------------------------------------------------
// TrainingRevision（训练修订版本，不可变资源，通过内容哈希标识）
// ---------------------------------------------------------------------------

/// 触发微调配置调整的执行主体角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    /// 人工操作员
    User,
    /// 自动化专家规则引擎
    Rule,
    /// AI 智能体决策
    Ai,
}

/// 调整配置时保留的训练运行时内部状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRetention {
    /// 是否保留模型权重参数
    pub model: bool,
    /// 是否保留优化器（Optimizer，如 AdamW 的动量状态）
    pub optimizer: bool,
    /// 是否保留学习率调度器状态 (LR Scheduler)
    pub scheduler: bool,
    /// 是否保留混合精度梯度缩放器 (GradScaler)
    pub grad_scaler: bool,
}

/// 训练修订记录：不可变事件审计日志，记录训练过程中发生超参数动态调整时的全量状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingRevision {
    /// 自动计算的内容标识符（排除在哈希原像之外）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision_id: Option<String>,
    /// 关联的训练运行任务全局 ID
    pub run_id: String,
    /// 父级修订版本 ID（用于构成调整历史链条，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_revision_id: Option<String>,
    /// 调整原因说明（例如：`Loss spike detected, reducing learning rate`）
    pub reason: String,
    /// 触发调整时的输入指标快照（如 loss, grad_norm, lr 等）
    pub input_metrics: ObjectSnapshot,
    /// 触发调整所命中的决策规则名称
    pub decision_rule: String,
    /// 调整前的配置快照
    pub config_before: ObjectSnapshot,
    /// 调整后的配置快照
    pub config_after: ObjectSnapshot,
    /// 恢复/回滚时所基于的基础检查点 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_checkpoint_id: Option<String>,
    /// 状态保留策略
    pub state_retention: StateRetention,
    /// 金丝雀试跑验证结果快照（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub canary_result: Option<ObjectSnapshot>,
    /// 调整前后的性能指标变化差异（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub performance_delta: Option<ObjectSnapshot>,
    /// 该修订版本是否已被标记为回滚
    pub rolled_back: bool,
    /// 变更触发者
    pub actor: Actor,
}

// ---------------------------------------------------------------------------
// CheckpointMetadata（检查点元数据，不可变资源）
// ---------------------------------------------------------------------------

/// 训练检查点元数据：记录训练过程中保存的模型权重快照信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// 自动计算的内容标识符（排除在哈希原像之外）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkpoint_id: Option<String>,
    /// 关联的训练任务 ID
    pub run_id: String,
    /// 检查点对应的全局训练步数 (Global Step)
    pub step: u64,
    /// 检查点对应的训练轮次 (Epoch，可选)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub epoch: Option<u64>,
    /// 权重存储路径（本地路径或 S3 URI，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    /// 权重文件的完整性校验摘要（例如 SHA-256）
    pub digest: String,
    /// 保存该检查点时的评估指标快照 (eval_loss, accuracy 等)
    pub metrics: ObjectSnapshot,
    /// 产生该检查点时的训练修订版本 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision_id: Option<String>,
    /// 文件总大小（字节数，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
}
