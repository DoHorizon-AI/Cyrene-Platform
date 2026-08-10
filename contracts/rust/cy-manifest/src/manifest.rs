//! CYRENE 核心契约数据结构定义。
//!
//! 【与 JSON Schema 的关系】
//! 本模块中的结构体是 `schemas/manifests/` 下 JSON Schema 规范在 Rust 语言中的镜像实现。
//! 清单（Manifest）是平台各子系统（调度器、节点守护进程、沙箱、插件系统）之间通信的不可变核心数据凭据。

use std::collections::BTreeMap;

use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// 自由格式的对象快照字典类型（用于存储指标、训练超参数配置等动态字典）。
///
/// 使用 `serde_json::Map<String, Value>` 存储，在进行规范哈希计算时键名会自动按字母序排序，
/// 从而保证哈希结果的跨平台唯一确定性。
pub type ObjectSnapshot = Map<String, Value>;

// ---------------------------------------------------------------------------
// HardwareManifest（硬件清单）
// ---------------------------------------------------------------------------

/// 操作系统环境信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsInfo {
    /// 操作系统名称（例如："Ubuntu"、"Linux"）
    pub name: String,
    /// 内核版本（例如："5.15.0-88-generic"）
    pub kernel: String,
    /// C 标准库 (glibc) 版本（例如："2.35"）
    pub glibc: String,
}

/// CPU 架构与核心信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuInfo {
    /// CPU 指令集架构（例如："x86_64"、"aarch64"）
    pub arch: String,
    /// 物理 CPU 核心数
    pub cores: u32,
    /// 逻辑线程数（超线程技术下的逻辑处理器数，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub threads: Option<u32>,
}

/// 单张/组 GPU 规格信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// GPU 具体型号（例如："NVIDIA A100-SXM4-80GB"）
    pub model: String,
    /// 同型号 GPU 的数量
    pub count: u32,
    /// 单卡显存容量（单位：GB）
    pub vram_gb: f64,
    /// GPU 计算能力等级（Compute Capability，例如："8.0" 代表 Ampere 架构，"9.0" 代表 Hopper 架构）
    pub compute_capability: String,
}

/// GPU 间互联通信拓扑与 PCIe 特征
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interconnect {
    /// 是否支持并启用了 NVLink 高速卡间互联
    pub nvlink: bool,
    /// PCIe 总线代数（例如：4 代表 PCIe 4.0，5 代表 PCIe 5.0，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pcie_gen: Option<u32>,
}

/// 硬件层面对各种浮点数值精度的原生支持情况
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrecisionSupport {
    /// 是否支持 Bfloat16 精度（适合大模型训练，动态范围大）
    pub bf16: bool,
    /// 是否支持 IEEE FP16 半精度
    pub fp16: bool,
    /// 是否支持 FP8 精度（如 Hopper/Ada 架构的加速量化推演）
    pub fp8: bool,
}

/// 节点硬件清单：描述单台计算节点的完整物理硬件与驱动状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareManifest {
    /// 操作系统信息
    pub os: OsInfo,
    /// CPU 规格
    pub cpu: CpuInfo,
    /// 物理内存总量（GB）
    pub memory_gb: f64,
    /// 本地存储可用容量（GB）
    pub disk_gb: f64,
    /// GPU 设备列表
    pub gpus: Vec<GpuInfo>,
    /// GPU 驱动版本（例如："550.90.07"）
    pub driver_version: String,
    /// 驱动所支持的最高 CUDA 运行库版本（例如："12.4"）
    pub cuda_max_supported: String,
    /// 卡间互联拓扑
    pub interconnect: Interconnect,
    /// 数值精度支持特性
    pub precision_support: PrecisionSupport,
}

// ---------------------------------------------------------------------------
// ModelManifest（模型清单）
// ---------------------------------------------------------------------------

/// 神经网络模型权重的数值精度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightPrecision {
    /// 32位单精度浮点
    Fp32,
    /// 16位半精度浮点
    Fp16,
    /// 16位 Brain 浮点
    Bf16,
    /// 8位浮点
    Fp8,
    /// 8位有符号整数
    Int8,
    /// 4位整数
    Int4,
}

/// 权重文件的存储格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    /// HuggingFace Safetensors 零拷贝安全权重格式
    Safetensors,
    /// PyTorch 原生序列化格式 (.pt / .bin)
    Pytorch,
    /// llama.cpp GGUF 格式（适合 CPU/轻量级量化推理）
    Gguf,
    /// 激活感知权重量化格式 (AWQ)
    Awq,
    /// 精确通用量化格式 (GPTQ)
    Gptq,
    /// ONNX 开放神经网络交换格式
    Onnx,
}

/// 模型量化技术方案
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quantization {
    /// 无额外量化
    None,
    /// AWQ 4-bit 量化
    Awq,
    /// GPTQ 4-bit 量化
    Gptq,
    /// bitsandbytes NF4 (常用于 QLoRA 微调)
    BnbNf4,
    /// bitsandbytes 8-bit 量化
    BnbInt8,
    /// FP8 量化
    Fp8,
}

/// 预估的显存（VRAM）占用开销
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VramEstimate {
    /// 训练阶段预估显存开销（GB，包含激活值、梯度和优化器状态）
    pub train_gb: f64,
    /// 推理服务阶段预估显存开销（GB，包含 KV Cache）
    pub infer_gb: f64,
}

/// 模型清单：完整描述一个大语言模型或多模态模型的静态规格
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// 模型骨干网络架构（例如："llama", "qwen2", "mistral"）
    pub architecture: String,
    /// 模型参数量（例如：7_000_000_000 代表 7B 参数）
    pub params: u64,
    /// 权重的原始存储精度
    pub weight_precision: WeightPrecision,
    /// 最大上下文窗口长度（Token 数，例如：4096, 32768）
    pub context_length: u32,
    /// 权重文件格式
    pub format: ModelFormat,
    /// 是否需要执行模型仓库中的自定义 Python 代码 (trust_remote_code)
    pub remote_code: bool,
    /// 分词器类型或标识符（例如："AutoTokenizer", "tiktoken"）
    pub tokenizer: String,
    /// 对话模板（Jinja2 模板字符串，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chat_template: Option<String>,
    /// 量化方法（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub quantization: Option<Quantization>,
    /// 显存预估信息
    pub vram_estimate: VramEstimate,
}

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
// RuntimeManifest（运行时环境清单）
// ---------------------------------------------------------------------------

/// 运行时所依赖的目标硬件画像
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// 目标 GPU 型号
    pub gpu_model: String,
    /// GPU 数量
    pub gpu_count: u32,
    /// 显存大小（GB）
    pub vram_gb: f64,
    /// 驱动版本
    pub driver_version: String,
    /// 支持的最高 CUDA 版本
    pub cuda_max_supported: String,
}

/// 分布式与参数高效训练策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrainingStrategy {
    /// 无（用于推理服务）
    None,
    /// 全量参数微调 (Full Fine-Tuning)
    Full,
    /// 低秩自适应微调 (LoRA)
    Lora,
    /// 量化低秩自适应微调 (QLoRA, 4-bit 量化基座)
    Qlora,
    /// DeepSpeed ZeRO 阶段分布式训练
    Deepspeed,
    /// PyTorch 完全分片数据并行 (FSDP)
    Fsdp,
}

/// 运行时环境的认证与验证等级阶梯（Evidence Ladder）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationLevel {
    /// 声明级别（仅用户或静态规则声明，尚未解析）
    Declared,
    /// 解析级别（依赖版本与兼容性规则校验通过）
    Resolved,
    /// 构建级别（容器镜像构建完成）
    Built,
    /// 冒烟测试通过（基础启动与 CUDA 探测正常）
    SmokeTested,
    /// 模型加载成功（权重已成功装载进显存）
    ModelLoaded,
    /// 逻辑验证通过（基本功能测试无误）
    Validated,
    /// 性能基准测试通过（吞吐与延迟数据已采录）
    Benchmarked,
    /// 正式认证就绪（达到生产上线标准）
    Certified,
}

/// 运行时清单：描述确定性的执行容器/依赖栈环境
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    /// 自动计算的内容标识符（例如："sha256:942031..."）。
    /// 注意：在计算自身哈希时，该字段会被排除在外。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime_id: Option<String>,
    /// 任务类型
    pub workload: Workload,
    /// 硬件画像
    pub hardware_profile: HardwareProfile,
    /// Python 版本（例如："3.12.13"）
    pub python: String,
    /// CUDA 运行时版本（例如："12.4.1"）
    pub cuda_runtime: String,
    /// PyTorch 版本（例如："2.4.0"）
    pub torch: String,
    /// 框架与三方库依赖版本映射表（例如：{"transformers": "4.44.2", "peft": "0.12.0"}）
    #[serde(default)]
    pub frameworks: BTreeMap<String, String>,
    /// 计算权重精度
    pub precision: WeightPrecision,
    /// 训练策略
    pub training_strategy: TrainingStrategy,
    /// 基础镜像摘要（OCI Image Digest）
    pub base_image_digest: String,
    /// 当前达到的验证等级
    pub validation_level: ValidationLevel,
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
    /// 调整原因说明（例如："Loss spike detected, reducing learning rate"）
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

// ---------------------------------------------------------------------------
// ArtifactManifest（产物清单，不可变资源）
// ---------------------------------------------------------------------------

/// 构建产物类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// 完整模型权重
    Model,
    /// 处理后的数据集
    Dataset,
    /// 中间训练检查点
    Checkpoint,
    /// 合并 LoRA 后的权重
    Merged,
    /// 量化产物
    Quantized,
    /// 分析评估报告
    Report,
}

/// 产物血缘追踪记录（Lineage）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lineage {
    /// 基座模型版本 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_model_revision: Option<String>,
    /// 输入数据集的内容摘要（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset_digest: Option<String>,
    /// 产生该产物所用的运行时环境 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime_id: Option<String>,
    /// 训练修订链条历史 ID 列表
    pub revision_chain: Vec<String>,
    /// 关联的检查点 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkpoint_id: Option<String>,
}

/// 产物清单：完整描述平台生成或存储的任意资产及其血缘
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// 自动计算的内容标识符（排除在哈希原像之外）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_id: Option<String>,
    /// 产物类别
    pub kind: ArtifactKind,
    /// 数据来源（S3 / HTTP / Local 路径，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
    /// 完整性校验哈希
    pub integrity: String,
    /// 字节大小（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
    /// 血缘谱系
    pub lineage: Lineage,
    /// 引用计数（用于垃圾回收 GC，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ref_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// PluginManifest（插件清单）
// ---------------------------------------------------------------------------

/// 插件发布版本级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    /// 社区开源版本
    Community,
    /// 专业/企业商业版本
    Pro,
}

impl Edition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Edition::Community => "community",
            Edition::Pro => "pro",
        }
    }
}

impl std::fmt::Display for Edition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 插件运行宿主环境形式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// 作为独立子进程运行的 Python 插件 (stdio/IPC 通信)
    SubprocessPython,
    /// 作为独立子进程运行的 JVM 插件 (stdio/IPC 通信)
    SubprocessJvm,
    /// 长期驻留独立系统服务
    Service,
}

impl Runtime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Runtime::SubprocessPython => "subprocess-python",
            Runtime::SubprocessJvm => "subprocess-jvm",
            Runtime::Service => "service",
        }
    }

    pub fn is_subprocess(&self) -> bool {
        matches!(self, Runtime::SubprocessPython | Runtime::SubprocessJvm)
    }
}

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 插件异常退出时的重启策略
#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    /// 绝不重启
    Never,
    /// 异常失败时重启（带指数退避）
    OnFailure {
        /// 最大允许连续重启次数
        max_restarts: u32,
        /// 初始退避等待时长（毫秒）
        min_backoff_ms: u64,
        /// 最大退避等待时长（毫秒）
        max_backoff_ms: u64,
        /// 指数退避倍率因子
        factor: f64,
    },
    /// 总是重启
    Always,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy::OnFailure {
            max_restarts: 3,
            min_backoff_ms: 500,
            max_backoff_ms: 30000,
            factor: 2.0,
        }
    }
}

impl Serialize for RestartPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Never => "never",
            Self::OnFailure { .. } => "on-failure",
            Self::Always => "always",
        })
    }
}

impl<'de> Deserialize<'de> for RestartPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RestartPolicyVisitor;

        impl<'de> Visitor<'de> for RestartPolicyVisitor {
            type Value = RestartPolicy;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("never, on-failure, or always")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "never" => Ok(RestartPolicy::Never),
                    "on-failure" => Ok(RestartPolicy::default()),
                    "always" => Ok(RestartPolicy::Always),
                    _ => Err(E::custom(format!("unsupported restart policy: {value}"))),
                }
            }
        }

        deserializer.deserialize_str(RestartPolicyVisitor)
    }
}

/// 插件启动命令与进程参数配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginLaunch {
    /// 执行文件路径或二进制名称（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub executable: Option<String>,
    /// 命令行启动参数列表
    #[serde(default)]
    pub args: Vec<String>,
    /// 通信传输机制（例如："stdio", "uds", "tcp"，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub transport: Option<String>,
    /// 启动握手超时时间（毫秒，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub startup_timeout_ms: Option<u64>,
    /// 优雅退出超时时间（毫秒，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shutdown_timeout_ms: Option<u64>,
}

/// 插件安全权限申请声明
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPermissions {
    /// 允许读取的文件系统路径模式
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    /// 允许写入的文件系统路径模式
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    /// 允许出站网络访问的域名/IP 列表
    #[serde(default)]
    pub network: Vec<String>,
    /// 是否需要 GPU 访问权限
    #[serde(default)]
    pub gpu: bool,
    /// 是否允许衍生子进程
    #[serde(default)]
    pub process_spawn: bool,
}

/// 插件硬件资源配额与通信限制
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginResources {
    /// 最大允许使用的内存上限（MB，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_mb: Option<u64>,
    /// 单个 IPC 消息包最大字节数（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_message_bytes: Option<u64>,
}

/// 插件包分发与完整性签名信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPackage {
    /// 插件压缩包 SHA-256 校验和（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
    /// 数字签名（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
    /// 支持的目标操作系统（例如：["linux", "windows"]）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub target_os: Vec<String>,
    /// 支持的目标架构（例如：["x86_64", "aarch64"]）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub target_arch: Vec<String>,
}

/// 插件基础元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    /// 插件唯一标识符（例如："com.cyrene.nvidia.probe"）
    pub id: String,
    /// 插件名称
    #[serde(default)]
    pub name: String,
    /// 插件版本号（语义化版本，例如："1.0.0"）
    pub version: String,
    /// 兼容的平台 API 版本（例如："1.0"）
    pub api_version: String,
    /// 插件类型分类（保持为 String 避免包循环依赖）
    pub kind: String,
    /// 发布版本（Community 或 Pro）
    pub edition: Edition,
    /// 运行宿主方式（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime: Option<Runtime>,
    /// 是否需要商业许可证门禁验证
    #[serde(default)]
    pub license_gate: bool,
    /// 插件入口类/函数路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub entrypoint: Option<String>,
    /// 插件功能描述（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// 插件作者（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub author: Option<String>,
    /// 开源/商业协议（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub license: Option<String>,
    /// 源码目标路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_target: Option<String>,
    /// 状态（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    /// 通信协议版本号（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub protocol_version: Option<u32>,
    /// 作用域范围（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope: Option<String>,
    /// 进程故障重启策略
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

impl PluginMetadata {
    pub fn plugin_id(&self) -> &str {
        &self.id
    }
}

/// 插件依赖关系声明
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginDependencies {
    /// 必须前置安装的其他插件 ID 列表
    #[serde(default, alias = "other_plugins")]
    pub required_plugins: Vec<String>,
    /// 可选集成的插件 ID 列表
    #[serde(default)]
    pub optional_plugins: Vec<String>,
    /// 互斥冲突的插件 ID 列表
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// 依赖的系统能力特性
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// 依赖的系统级动态库/可执行文件
    #[serde(default)]
    pub system_dependencies: Vec<String>,
    /// 依赖的 Python 第三方包
    #[serde(default)]
    pub python_packages: Vec<String>,
}

/// 插件所宣称支持的能力特性清单
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginCapabilitiesManifest {
    /// 支持的硬件型号列表
    #[serde(default)]
    pub supported_hardware: Vec<String>,
    /// 支持的计算精度
    #[serde(default)]
    pub supported_precisions: Vec<String>,
    /// 支持的量化方法
    #[serde(default)]
    pub supported_quantizations: Vec<String>,
    /// 是否支持流式传输 (Streaming)
    #[serde(default)]
    pub supports_streaming: bool,
    /// 扩展特性标签列表
    #[serde(default)]
    pub features: Vec<String>,
}

/// 组合包插件内包含的子组件定义
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginComponent {
    /// 组件唯一标识 ID
    pub id: String,
    /// 组件类型（对应扩展点分类，例如 "probe", "model-analyzer"）
    pub kind: String,
    /// 发布版本（Community 或 Pro）
    pub edition: Edition,
    /// 组件版本号
    pub version: String,
    /// 运行宿主环境方式（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime: Option<Runtime>,
    /// 是否需要商业许可证门禁验证
    #[serde(default)]
    pub license_gate: bool,
    /// 声明的能力特性标识列表
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 组件状态（例如 "active", "deprecated"，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    /// 源码目标路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_target: Option<String>,
}

/// 完整插件清单（PluginManifest）：描述插件元数据、能力、依赖、安全策略及进程启动参数
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// 基础元数据
    pub plugin: PluginMetadata,
    /// 能力声明
    #[serde(default)]
    pub capabilities: PluginCapabilitiesManifest,
    /// 依赖关系
    #[serde(default)]
    pub dependencies: PluginDependencies,
    /// 包含的组件列表
    #[serde(default)]
    pub components: Vec<PluginComponent>,

    /// 进程启动配置（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub launch: Option<PluginLaunch>,
    /// 权限声明（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub permissions: Option<PluginPermissions>,
    /// 资源限制（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resources: Option<PluginResources>,
    /// 打包分发属性（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub package: Option<PluginPackage>,
}
