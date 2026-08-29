//! 模型契约数据结构定义。
//!
//! 包含数值精度、权重格式、量化方法、显存预估及模型清单等静态规格定义。

use serde::{Deserialize, Serialize};

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
