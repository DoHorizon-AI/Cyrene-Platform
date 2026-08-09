pub mod builtin;
use async_trait::async_trait;
use cy_manifest::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Canonical plugin taxonomy (`kind`). See `docs/PLUGIN_SPEC.md` §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    Probe,
    ModelAnalyzer,
    CompatRule,
    RuntimeBuilder,
    ExecutionEngine,
    TrainingBackend,
    Quantization,
    GatewayFilter,
    Notification,
    Storage,
    Bundle,
    Service,
    Library,
    PythonPackage,
    ProtocolAndServices,
    DeploymentAssets,
}

impl PluginKind {
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

/// Capabilities declared by a plugin (`[capabilities]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilities {
    #[serde(default)]
    pub supported_hardware: Vec<String>,
    #[serde(default)]
    pub supported_precisions: Vec<String>,
    #[serde(default)]
    pub supported_quantizations: Vec<String>,
    #[serde(default)]
    pub supports_streaming: bool,
    #[serde(default)]
    pub features: Vec<String>,
}

pub const PLUGIN_API_VERSION: &str = "1.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    Unavailable(String),
    Connect(String),
    Execution(String),
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

#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> PluginKind;
    fn api_version(&self) -> &str;
    fn capabilities(&self) -> &PluginCapabilities;
}

#[async_trait]
pub trait Probe: Plugin {
    async fn detect_hardware(&self) -> Result<HardwareManifest, PluginError>;
}

#[async_trait]
pub trait ModelAnalyzer: Plugin {
    async fn analyze_model(
        &self,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<VramEstimate, PluginError>;
}

#[async_trait]
pub trait CompatRule: Plugin {
    async fn evaluate_compatibility(
        &self,
        hardware: &HardwareManifest,
        model: &ModelManifest,
        workload: &WorkloadRequest,
    ) -> Result<WhyReport, PluginError>;
}

#[async_trait]
pub trait RuntimeBuilder: Plugin {
    async fn build_runtime(
        &self,
        workload: &WorkloadRequest,
        hardware: &HardwareManifest,
        model: &ModelManifest,
    ) -> Result<RuntimeManifest, PluginError>;
}

#[async_trait]
pub trait ExecutionEngine: Plugin {
    async fn execute_inference(
        &self,
        runtime: &RuntimeManifest,
        model: &ModelManifest,
        prompt: &str,
    ) -> Result<String, PluginError>;
}

#[async_trait]
pub trait TrainingBackend: Plugin {
    async fn run_training_step(
        &self,
        runtime: &RuntimeManifest,
        revision: &TrainingRevision,
    ) -> Result<CheckpointMetadata, PluginError>;
}

#[async_trait]
pub trait Quantization: Plugin {
    async fn quantize_model(
        &self,
        model: &ModelManifest,
        target_precision: WeightPrecision,
    ) -> Result<ArtifactManifest, PluginError>;
}

#[async_trait]
pub trait GatewayFilter: Plugin {
    async fn filter_request(
        &self,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> Result<bool, PluginError>;
}

#[async_trait]
pub trait Notification: Plugin {
    async fn send_notification(
        &self,
        topic: &str,
        message: &str,
        level: &str,
    ) -> Result<(), PluginError>;
}

#[async_trait]
pub trait Storage: Plugin {
    async fn store_artifact(
        &self,
        artifact: &ArtifactManifest,
        data: &[u8],
    ) -> Result<String, PluginError>;

    async fn fetch_artifact(&self, artifact_id: &str) -> Result<Vec<u8>, PluginError>;
}
