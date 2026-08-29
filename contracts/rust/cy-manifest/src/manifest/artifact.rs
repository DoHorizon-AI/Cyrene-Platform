//! 构建产物契约数据结构定义。
//!
//! 包含构建产物类别、血缘追踪记录（Lineage）及构建产物清单（ArtifactManifest）。

use serde::{Deserialize, Serialize};

/// 构建产物类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// 通用不可变产物
    Generic,
    /// 完整模型权重
    Model,
    /// 处理后的数据集
    Dataset,
    /// 中间训练检查点
    Checkpoint,
    /// 训练规格
    TrainingSpec,
    /// 指标数据
    Metrics,
    /// 合并 LoRA 后的权重
    Merged,
    /// 量化产物
    Quantized,
    /// 分析评估报告
    Report,
}

/// Provider-neutral artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Provider-neutral content URI.
    pub uri: String,
    /// SHA-256 digest used as the immutable identity reference.
    pub digest: String,
    /// Referenced artifact size in bytes.
    pub size_bytes: u64,
    /// Artifact category.
    pub kind: ArtifactKind,
    /// Optional provider manifest digest for multi-file artifacts.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub manifest_digest: Option<String>,
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
    /// Optional schema version for forward-compatible projections.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub schema_version: Option<String>,
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
