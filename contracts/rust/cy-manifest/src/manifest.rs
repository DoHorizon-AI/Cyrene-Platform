//! Core manifest types.
//!
//! These mirror the JSON Schemas under `schemas/manifests/` (source of truth).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A free-form object snapshot (metrics, config, etc.). Stored as a JSON object
/// so it canonicalizes deterministically (keys are sorted at hash time).
pub type ObjectSnapshot = Map<String, Value>;

// ---------------------------------------------------------------------------
// HardwareManifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsInfo {
    pub name: String,
    pub kernel: String,
    pub glibc: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuInfo {
    pub arch: String,
    pub cores: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub threads: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub model: String,
    pub count: u32,
    pub vram_gb: f64,
    pub compute_capability: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interconnect {
    pub nvlink: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pcie_gen: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrecisionSupport {
    pub bf16: bool,
    pub fp16: bool,
    pub fp8: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareManifest {
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub memory_gb: f64,
    pub disk_gb: f64,
    pub gpus: Vec<GpuInfo>,
    pub driver_version: String,
    pub cuda_max_supported: String,
    pub interconnect: Interconnect,
    pub precision_support: PrecisionSupport,
}

// ---------------------------------------------------------------------------
// ModelManifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightPrecision {
    Fp32,
    Fp16,
    Bf16,
    Fp8,
    Int8,
    Int4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    Safetensors,
    Pytorch,
    Gguf,
    Awq,
    Gptq,
    Onnx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quantization {
    None,
    Awq,
    Gptq,
    BnbNf4,
    BnbInt8,
    Fp8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VramEstimate {
    pub train_gb: f64,
    pub infer_gb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelManifest {
    pub architecture: String,
    pub params: u64,
    pub weight_precision: WeightPrecision,
    pub context_length: u32,
    pub format: ModelFormat,
    pub remote_code: bool,
    pub tokenizer: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chat_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub quantization: Option<Quantization>,
    pub vram_estimate: VramEstimate,
}

// ---------------------------------------------------------------------------
// WorkloadRequest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Workload {
    Finetune,
    Serve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Latency,
    Throughput,
    Cost,
    Quality,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Objective {
    pub priority: Priority,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constraints {
    pub max_gpu_count: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadRequest {
    pub model: String,
    pub workload: Workload,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset: Option<String>,
    pub objective: Objective,
    pub constraints: Constraints,
}

// ---------------------------------------------------------------------------
// RuntimeManifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub gpu_model: String,
    pub gpu_count: u32,
    pub vram_gb: f64,
    pub driver_version: String,
    pub cuda_max_supported: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrainingStrategy {
    None,
    Full,
    Lora,
    Qlora,
    Deepspeed,
    Fsdp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationLevel {
    Declared,
    Resolved,
    Built,
    SmokeTested,
    ModelLoaded,
    Validated,
    Benchmarked,
    Certified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    /// Computed content id. EXCLUDED from the canonical hash preimage.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime_id: Option<String>,
    pub workload: Workload,
    pub hardware_profile: HardwareProfile,
    pub python: String,
    pub cuda_runtime: String,
    pub torch: String,
    #[serde(default)]
    pub frameworks: BTreeMap<String, String>,
    pub precision: WeightPrecision,
    pub training_strategy: TrainingStrategy,
    pub base_image_digest: String,
    pub validation_level: ValidationLevel,
}

// ---------------------------------------------------------------------------
// WhyReport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Chosen,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub subject: String,
    pub verdict: Verdict,
    pub rationale: String,
    pub evidence: Vec<String>,
    pub confidence: Confidence,
    /// Strength of the backing evidence on the project's evidence ladder.
    pub evidence_level: ValidationLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhyReport {
    pub decisions: Vec<Decision>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// ValidationResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Reference to the validated RuntimeManifest ("sha256:<hex>").
    pub runtime_id: String,
    pub level: ValidationLevel,
    pub checks: Vec<Check>,
    pub evidence_level: ValidationLevel,
}

// ---------------------------------------------------------------------------
// TrainingRevision (immutable; content-hash id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Rule,
    Ai,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRetention {
    pub model: bool,
    pub optimizer: bool,
    pub scheduler: bool,
    pub grad_scaler: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingRevision {
    /// Computed content id. EXCLUDED from the canonical hash preimage.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision_id: Option<String>,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_revision_id: Option<String>,
    pub reason: String,
    pub input_metrics: ObjectSnapshot,
    pub decision_rule: String,
    pub config_before: ObjectSnapshot,
    pub config_after: ObjectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_checkpoint_id: Option<String>,
    pub state_retention: StateRetention,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub canary_result: Option<ObjectSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub performance_delta: Option<ObjectSnapshot>,
    pub rolled_back: bool,
    pub actor: Actor,
}

// ---------------------------------------------------------------------------
// CheckpointMetadata (immutable; content-hash id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// Computed content id. EXCLUDED from the canonical hash preimage.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkpoint_id: Option<String>,
    pub run_id: String,
    pub step: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    pub digest: String,
    pub metrics: ObjectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// ArtifactManifest (immutable; content-hash id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Model,
    Dataset,
    Checkpoint,
    Merged,
    Quantized,
    Report,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lineage {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_model_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime_id: Option<String>,
    pub revision_chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// Computed content id. EXCLUDED from the canonical hash preimage.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_id: Option<String>,
    pub kind: ArtifactKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
    pub integrity: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
    pub lineage: Lineage,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ref_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// PluginManifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    Community,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    SubprocessPython,
    SubprocessJvm,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure {
        max_restarts: u32,
        min_backoff_ms: u64,
        max_backoff_ms: u64,
        factor: f64,
    },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginLaunch {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub startup_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shutdown_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPermissions {
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub gpu: bool,
    #[serde(default)]
    pub process_spawn: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginResources {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_message_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPackage {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target_os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target_arch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginMetadata {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    pub version: String,
    pub api_version: String,
    pub kind: String, // Kept as string to avoid cy_platform_api dependency in cy-manifest, since cy_platform_api depends on cy-manifest
    pub edition: Edition,
    #[serde(default)]
    pub runtime: Option<Runtime>,
    #[serde(default)]
    pub license_gate: bool,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub source_target: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

impl PluginMetadata {
    pub fn plugin_id(&self) -> &str {
        if let Some(ref id) = self.id {
            id.as_str()
        } else if !self.name.is_empty() {
            &self.name
        } else {
            "unknown"
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginDependencies {
    #[serde(default, alias = "other_plugins")]
    pub required_plugins: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub system_dependencies: Vec<String>,
    #[serde(default)]
    pub python_packages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilitiesManifest {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginComponent {
    pub id: String,
    pub kind: String,
    pub edition: Edition,
    pub version: String,
    #[serde(default)]
    pub runtime: Option<Runtime>,
    #[serde(default)]
    pub license_gate: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub source_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    pub plugin: PluginMetadata,
    #[serde(default)]
    pub capabilities: PluginCapabilitiesManifest,
    #[serde(default)]
    pub dependencies: PluginDependencies,
    #[serde(default)]
    pub components: Vec<PluginComponent>,

    // Additional requested fields per prompt
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub host_api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub launch: Option<PluginLaunch>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub permissions: Option<PluginPermissions>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resources: Option<PluginResources>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub package: Option<PluginPackage>,

    #[serde(default)]
    pub optional_plugins: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
}
