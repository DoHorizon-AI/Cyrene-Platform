// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/hardware.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 硬件契约数据结构定义。
//!
//! 包含操作系统、CPU、GPU、互联通信拓扑及节点硬件清单等物理计算资源模型。

use serde::{Deserialize, Serialize};

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
