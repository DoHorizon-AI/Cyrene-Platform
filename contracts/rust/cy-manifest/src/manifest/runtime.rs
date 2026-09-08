// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/runtime.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 运行时环境契约数据结构定义。
//!
//! 包含硬件画像、训练策略、验证等级阶梯及运行时环境清单（RuntimeManifest）。
//!
//! `MIGRATING_COMPATIBILITY`: the v0 AI-specific runtime projection is retained
//! for existing hash and typed-SPI consumers. New execution uses generic
//! Platform execution plans and Product-owned capability payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::WeightPrecision;
use super::training::Workload;

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
