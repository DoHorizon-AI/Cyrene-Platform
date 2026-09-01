//! CYRENE 平台扩展点 SPI 与插件分类契约 (Platform Extension Points & SPI).
//!
//! 【平台扩展架构】
//! 定义了平台与各功能插件之间交互的 10 大标准化扩展点 SPI Traits（Service Provider Interfaces）：
//! 1. [`Probe`][]: 宿主机物理硬件与环境探测；
//! 2. [`ModelAnalyzer`][]: 模型权重与架构静态/动态分析；
//! 3. [`CompatRule`][]: 硬件与算子兼容性规则引擎；
//! 4. [`RuntimeBuilder`][]: 推理执行运行时镜像/环境构建器；
//! 5. [`ExecutionEngine`]: 推理执行引擎（如 vLLM / TensorRT-LLM）；
//! 6. [`TrainingBackend`][]: 训练与微调后端引擎；
//! 7. [`Quantization`]: 模型量化压缩器（AWQ, GPTQ, SmoothQuant 等）；
//! 8. [`GatewayFilter`][]: 请求网关路由、鉴权与前置/后置过滤器；
//! 9. [`Notification`][]: 告警、事件与状态通知管道；
//! 10. [`Storage`][]: 产物（模型权重、数据集、检查点）存储读写适配器。

pub mod builtin;
pub mod media;
pub mod official_manifest;
pub mod plugin;
pub mod worker;
use async_trait::async_trait;
pub use cy_manifest::*;
pub use media::{
    AudioInput, CancellationToken, CanonicalAudioProfile, EncodedAudio, EncodedImage,
    INSPECT_IMAGE_OPERATION, ImageFormat, ImageInput, ImageInspection, ImageOrientation,
    InspectImageRequest, MEDIA_PROCESSOR_INTERFACE_V1, MEDIA_PROCESSOR_V1, MediaProcessor,
    MediaProcessorError, NORMALIZE_AUDIO_OPERATION, NeverCancelled, NormalizeAudioRequest,
    NormalizedAudio, ResizeOptions, TRANSFORM_IMAGE_OPERATION, TransformImageRequest,
    TransformedImage,
};
pub use official_manifest::{OfficialPluginManifest, normalize_official_manifest};
pub use plugin::{
    CapabilityBinding, CapabilityRegistry, CapabilityResolutionError, CapabilityResolver,
    ResolvedCapabilityTarget,
};
pub use worker::{
    ApplicationEventError, ApplicationEventStreamEndReason, ApplicationEventStreamTermination,
    ApplicationEventSubscription, AtomicCancellationToken, CapabilityWorkerActivator,
    CapabilityWorkerClient, WorkerApplicationEvent, WorkerActivationOptions,
    WorkerInvocationResult,
    WorkerMediaProcessor, WorkerTerminalError, DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY,
    MAX_APPLICATION_EVENT_BUFFER_CAPACITY,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 规范化插件分类字典枚举 (`kind`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    /// 1. 硬件探针
    Probe,
    /// 2. 模型分析器
    ModelAnalyzer,
    /// 3. 兼容性规则
    CompatRule,
    /// 4. 运行时构建器
    RuntimeBuilder,
    /// 5. 执行引擎
    ExecutionEngine,
    /// 6. 训练后端
    TrainingBackend,
    /// 7. 量化工具
    Quantization,
    /// 8. 网关过滤器
    GatewayFilter,
    /// 9. 通知器
    Notification,
    /// 10. 存储适配器
    Storage,
    /// 组合插件包
    Bundle,
    /// 独立系统服务
    Service,
    /// 代码库依赖
    Library,
    /// Python 包依赖
    PythonPackage,
    /// 协议与服务定义
    ProtocolAndServices,
    /// 部署资源产物
    DeploymentAssets,
}

impl PluginKind {
    /// 获取规范字符串字面量
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginKind::Probe => "probe",
            PluginKind::ModelAnalyzer => "model-analyzer",
            PluginKind::CompatRule => "compat-rule",
            PluginKind::RuntimeBuilder => "runtime-builder",
            PluginKind::ExecutionEngine => "execution-engine",
            PluginKind::TrainingBackend => "training-backend",
            PluginKind::Quantization => "quantization",
            PluginKind::GatewayFilter => "gateway-filter",
            PluginKind::Notification => "notification",
            PluginKind::Storage => "storage",
            PluginKind::Bundle => "bundle",
            PluginKind::Service => "service",
            PluginKind::Library => "library",
            PluginKind::PythonPackage => "python-package",
            PluginKind::ProtocolAndServices => "protocol-and-services",
            PluginKind::DeploymentAssets => "deployment-assets",
        }
    }

    /// 判断当前分类是否为平台 10 大核心扩展点之一
    pub fn is_extension_point(&self) -> bool {
        matches!(
            self,
            PluginKind::Probe
                | PluginKind::ModelAnalyzer
                | PluginKind::CompatRule
                | PluginKind::RuntimeBuilder
                | PluginKind::ExecutionEngine
                | PluginKind::TrainingBackend
                | PluginKind::Quantization
                | PluginKind::GatewayFilter
                | PluginKind::Notification
                | PluginKind::Storage
        )
    }
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 插件声明的能力特性集 (`[capabilities]`)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilities {
    /// 支持的硬件型号列表
    #[serde(default)]
    pub supported_hardware: Vec<String>,
    /// 支持的精度格式（如 "fp16", "bf16", "fp8"）
    #[serde(default)]
    pub supported_precisions: Vec<String>,
    /// 支持的量化算法（如 "awq", "gptq"）
    #[serde(default)]
    pub supported_quantizations: Vec<String>,
    /// 是否支持流式传输
    #[serde(default)]
    pub supports_streaming: bool,
    /// 扩展能力标签列表
    #[serde(default)]
    pub features: Vec<String>,
}

/// 平台当前插件 API 契约版本
pub const PLUGIN_API_VERSION: &str = "1.0";

/// 插件执行错误枚举
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// 插件不可用
    Unavailable(String),
    /// 连接通信异常
    Connect(String),
    /// 执行内部错误
    Execution(String),
    /// 输入参数无效
    Invalid(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Unavailable(m) => write!(f, "plugin unavailable: {m}"),
            PluginError::Connect(m) => write!(f, "connect failed: {m}"),
            PluginError::Execution(m) => write!(f, "execution failed: {m}"),
            PluginError::Invalid(m) => write!(f, "invalid input: {m}"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<PluginError> for String {
    fn from(e: PluginError) -> Self {
        e.to_string()
    }
}

/// 所有插件通用基础 Trait
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 插件唯一标识符
    fn id(&self) -> &str;
    /// 插件分类类别
    fn kind(&self) -> PluginKind;
    /// 插件遵循的 API 契约版本
    fn api_version(&self) -> &str;
    /// 插件能力特性声明
    fn capabilities(&self) -> &PluginCapabilities;
}

/// 扩展点 1: 硬件探针 (Probe)
#[async_trait]
pub trait Probe: Plugin {
    /// 探测宿主机硬件配置并返回不可变的 [`HardwareManifest`]
    async fn detect_hardware(&self) -> Result<HardwareManifest, PluginError>;
}

/// 扩展点 2: 模型分析器 (ModelAnalyzer)
#[async_trait]
pub trait ModelAnalyzer: Plugin {
    /// 静态/动态分析模型在特定工作负载下的显存消耗
    async fn analyze_model(
        &self,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<VramEstimate, PluginError>;
}

/// 扩展点 3: 兼容性规则引擎 (CompatRule)
#[async_trait]
pub trait CompatRule: Plugin {
    /// 评估硬件、模型与工作负载之间的兼容性，输出不可变决策证据链 [`WhyReport`]
    async fn evaluate_compatibility(
        &self,
        hardware: &HardwareManifest,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<WhyReport, PluginError>;
}

/// 扩展点 4: 运行时构建器 (RuntimeBuilder)
#[async_trait]
pub trait RuntimeBuilder: Plugin {
    /// 根据工作负载、硬件与模型组装可执行环境清单 [`RuntimeManifest`]
    async fn build_runtime(
        &self,
        workload: &WorkloadRequest,
        hardware: &HardwareManifest,
        model: &ModelManifest,
    ) -> Result<RuntimeManifest, PluginError>;
}

/// 扩展点 5: 执行引擎 (ExecutionEngine)
#[async_trait]
pub trait ExecutionEngine: Plugin {
    /// 执行大模型推理请求并返回生成结果文本
    async fn execute_inference(
        &self,
        runtime: &RuntimeManifest,
        model: &ModelManifest,
        prompt: &str,
    ) -> Result<String, PluginError>;
}

/// 扩展点 6: 训练后端 (TrainingBackend)
#[async_trait]
pub trait TrainingBackend: Plugin {
    /// 执行一步或多步训练调整，输出检查点元数据 [`CheckpointMetadata`]
    async fn run_training_step(
        &self,
        runtime: &RuntimeManifest,
        revision: &TrainingRevision,
    ) -> Result<CheckpointMetadata, PluginError>;
}

/// 扩展点 7: 模型量化器 (Quantization)
#[async_trait]
pub trait Quantization: Plugin {
    /// 对模型执行量化转换，输出产物清单 [`ArtifactManifest`]
    async fn quantize_model(
        &self,
        model: &ModelManifest,
        target_precision: WeightPrecision,
    ) -> Result<ArtifactManifest, PluginError>;
}

/// 扩展点 8: 网关过滤器 (GatewayFilter)
#[async_trait]
pub trait GatewayFilter: Plugin {
    /// 对入站 HTTP/RPC 请求进行前置过滤或鉴权拦截（返回 true 放行，false 拦截）
    async fn filter_request(
        &self,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> Result<bool, PluginError>;
}

/// 扩展点 9: 通知管道 (Notification)
#[async_trait]
pub trait Notification: Plugin {
    /// 发送平台告警或业务事件通知
    async fn send_notification(
        &self,
        topic: &str,
        message: &str,
        level: &str,
    ) -> Result<(), PluginError>;
}

/// 扩展点 10: 产物存储适配器 (Storage)
#[async_trait]
pub trait Storage: Plugin {
    /// 保存产物二进制数据，返回唯一产物 ID
    async fn store_artifact(
        &self,
        artifact: &ArtifactManifest,
        data: &[u8],
    ) -> Result<String, PluginError>;

    /// 根据产物 ID 读取二进制数据
    async fn fetch_artifact(&self, artifact_id: &str) -> Result<Vec<u8>, PluginError>;
}
