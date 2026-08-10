//! CYRENE 核心清单（Manifest）类型与确定性规范哈希定义。
//!
//! 【模块职责与架构定位】
//! 1. 契约数据结构源头：定义了平台核心实体（硬件、模型、运行时环境、训练修订、检查点、构建产物等）的 Rust 数据模型，
//!    与 `schemas/manifests/` 下的 JSON Schema 严格对齐。
//! 2. 确定性内容标识符（Content ID）：基于 RFC 8785 (JCS) 规范哈希，为不可变实体（如 Runtime、Revision、Checkpoint、Artifact）
//!    计算前缀为 `sha256:` 的内容寻址 ID。
//! 3. 避免递归哈希污染：在计算自身 ID 时，自动剥离对应的 ID 字段（例如计算 `runtime_id` 时剔除 JSON 中的 `runtime_id` 键），
//!    确保哈希原像（preimage）纯粹反映内容本身。

mod canonical;
mod manifest;

pub use canonical::{canonical_sha256_hex, canonicalize};
pub use manifest::*;

use serde::Serialize;
use serde_json::Value;

/// 支持确定性规范化和哈希计算的清单特征（Trait）。
///
/// 任何实现了 `Serialize` 的结构体都可以通过该特征获得：
/// - 规范化的 JSON AST (`canonical_value`)
/// - 符合 RFC 8785 的字节流 (`canonical_bytes`)
/// - SHA-256 小写 16 进制摘要 (`canonical_sha256_hex`)
pub trait Manifest: Serialize {
    /// 获取用于哈希计算的 JSON 抽象语法树（哈希原像 preimage）。
    ///
    /// 大多数清单直接序列化为其自身 JSON。而对于包含计算字段的清单（如 [`RuntimeManifest`]），
    /// 其自身的 `runtime_id` 字段会被移除，因为该 ID 是哈希的输出，不能作为其自身输入的组成部分。
    fn canonical_value(&self) -> Value {
        serde_json::to_value(self).expect("manifest is serializable to JSON")
    }

    /// 根据 `schemas/CANONICALIZATION.md` / RFC 8785 规则生成规范字节流。
    fn canonical_bytes(&self) -> Vec<u8> {
        canonicalize(&self.canonical_value())
    }

    /// 计算规范字节流的小写十六进制 SHA-256 摘要。
    fn canonical_sha256_hex(&self) -> String {
        canonical_sha256_hex(&self.canonical_value())
    }
}

// 为通用静态清单实现 Manifest 特征
impl Manifest for HardwareManifest {}
impl Manifest for ModelManifest {}
impl Manifest for WorkloadRequest {}
// WhyReport（决策原因报告）和 ValidationResult（校验结果）虽然不是不可变资源也不携带内容 ID，
// 但依然实现 Manifest 以便通过规范哈希快速进行变更检测（Change Detection）。
impl Manifest for WhyReport {}
impl Manifest for ValidationResult {}

/// 辅助函数：构建清单的规范 JSON 树，并剔除指定的自动计算 ID 键。
///
/// # 参数
/// * `manifest` - 待序列化的清单对象
/// * `id_key` - 需要剔除的字段名（例如 `"runtime_id"`、`"revision_id"`）
fn canonical_value_without(manifest: &impl Serialize, id_key: &str) -> Value {
    let mut value = serde_json::to_value(manifest).expect("manifest is serializable to JSON");
    if let Value::Object(ref mut map) = value {
        map.remove(id_key);
    }
    value
}

impl Manifest for RuntimeManifest {
    fn canonical_value(&self) -> Value {
        canonical_value_without(self, "runtime_id")
    }
}

impl Manifest for TrainingRevision {
    fn canonical_value(&self) -> Value {
        canonical_value_without(self, "revision_id")
    }
}

impl Manifest for CheckpointMetadata {
    fn canonical_value(&self) -> Value {
        canonical_value_without(self, "checkpoint_id")
    }
}

impl Manifest for ArtifactManifest {
    fn canonical_value(&self) -> Value {
        canonical_value_without(self, "artifact_id")
    }
}

/// 计算 [`RuntimeManifest`]（运行时环境清单）的唯一内容标识符 `runtime_id`。
///
/// 计算公式：`runtime_id = "sha256:" + hex(sha256(canonical_bytes(manifest_without_runtime_id)))`
pub fn runtime_id(manifest: &RuntimeManifest) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// 计算 [`TrainingRevision`]（训练微调修订记录）的唯一内容标识符 `revision_id`。
pub fn revision_id(manifest: &TrainingRevision) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// 计算 [`CheckpointMetadata`]（权重检查点元数据）的唯一内容标识符 `checkpoint_id`。
pub fn checkpoint_id(manifest: &CheckpointMetadata) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// 计算 [`ArtifactManifest`]（产物清单）的唯一内容标识符 `artifact_id`。
pub fn artifact_id(manifest: &ArtifactManifest) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// 从 JSON 字符串反序列化解析出 [`RuntimeManifest`]。
pub fn runtime_manifest_from_json(s: &str) -> Result<RuntimeManifest, serde_json::Error> {
    serde_json::from_str(s)
}

/// 从 YAML 字符串反序列化解析出 [`RuntimeManifest`]。
pub fn runtime_manifest_from_yaml(s: &str) -> Result<RuntimeManifest, serde_yaml::Error> {
    serde_yaml::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn installable_manifest_rejects_in_process_runtime() {
        let result = serde_json::from_str::<PluginManifest>(
            r#"{
                "plugin": {
                    "id": "com.cy.legacy",
                    "name": "legacy",
                    "version": "0.1.0",
                    "api_version": "1.0",
                    "kind": "probe",
                    "edition": "community",
                    "runtime": "in-proc-rust",
                    "crate": "legacy_probe"
                }
            }"#,
        );

        assert!(result.is_err(), "in-proc-rust must not be installable");
    }

    #[test]
    fn installable_manifest_accepts_jvm_subprocess() {
        let manifest = serde_json::from_str::<PluginManifest>(
            r#"{
                "plugin": {
                    "id": "com.cy.reference.jvm",
                    "name": "reference-jvm",
                    "version": "0.1.0",
                    "api_version": "1.0",
                    "kind": "probe",
                    "edition": "community",
                    "runtime": "subprocess-jvm",
                    "entrypoint": "reference.Main"
                }
            }"#,
        )
        .expect("subprocess-jvm manifest should parse");

        assert_eq!(manifest.plugin.runtime, Some(Runtime::SubprocessJvm));
        assert_eq!(
            manifest.plugin.entrypoint.as_deref(),
            Some("reference.Main")
        );
    }

    fn validate_plugin_schema(value: &Value) -> Result<(), Vec<String>> {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schemas/plugin.schema.json"))
                .expect("plugin schema must be valid JSON");
        let compiled = jsonschema::JSONSchema::compile(&schema)
            .expect("plugin schema must compile as JSON Schema");

        compiled
            .validate(value)
            .map_err(|errors| errors.map(|error| error.to_string()).collect())
    }

    #[test]
    fn plugin_manifest_fixture_passes_schema_and_rust_round_trip() {
        let value: Value = serde_json::from_str(include_str!(
            "../../../schemas/examples/plugin_manifest.example.json"
        ))
        .expect("plugin fixture must be valid JSON");

        validate_plugin_schema(&value).expect("plugin fixture must pass JSON Schema");
        let manifest: PluginManifest = serde_json::from_value(value).expect("fixture must parse");
        assert_eq!(manifest.plugin.id, "com.cyrene.reference.nvidia");
        assert!(matches!(
            &manifest.plugin.restart_policy,
            RestartPolicy::OnFailure { .. }
        ));
        assert_eq!(
            manifest.dependencies.optional_plugins,
            ["com.cyrene.telemetry"]
        );
        assert_eq!(
            manifest.package.as_ref().unwrap().target_arch,
            ["x86_64", "aarch64"]
        );

        let round_trip = serde_json::to_value(&manifest).expect("manifest must serialize");
        validate_plugin_schema(&round_trip).expect("Rust round-trip must pass JSON Schema");
    }

    #[test]
    fn plugin_manifest_rejects_schema_and_model_drift() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../schemas/examples/plugin_manifest.example.json"
        ))
        .expect("plugin fixture must be valid JSON");

        let mut unknown_field = fixture.clone();
        unknown_field["plugin"]["unexpected"] = Value::String("bad".to_string());
        assert!(validate_plugin_schema(&unknown_field).is_err());
        assert!(serde_json::from_value::<PluginManifest>(unknown_field).is_err());

        let mut wrong_package_type = fixture.clone();
        wrong_package_type["package"]["target_os"] = Value::String("linux".to_string());
        assert!(validate_plugin_schema(&wrong_package_type).is_err());
        assert!(serde_json::from_value::<PluginManifest>(wrong_package_type).is_err());

        let mut wrong_dependency_location = fixture;
        wrong_dependency_location["optional_plugins"] = serde_json::json!(["bad-location"]);
        assert!(validate_plugin_schema(&wrong_dependency_location).is_err());
        assert!(serde_json::from_value::<PluginManifest>(wrong_dependency_location).is_err());
    }

    #[test]
    fn reference_jvm_manifest_uses_supported_runtime() {
        let manifest = include_str!("../../../../examples/plugins/jvm/poc/plugin.toml");

        assert!(manifest
            .lines()
            .any(|line| line.trim() == r#"runtime = "subprocess-jvm""#));
        assert!(!manifest.contains("in-proc-rust"));
    }

    /// Known-answer for `schemas/examples/runtime_manifest.example.json`.
    ///
    /// If the sample or canonicalization scheme changes, update this value and
    /// version the contract before external bindings consume it.
    const KNOWN_RUNTIME_ID: &str =
        "sha256:9420315d29b089f5b95b3d39ee1d473426246cd32cd91631f37fdee596c46478";

    fn sample() -> RuntimeManifest {
        let mut frameworks = BTreeMap::new();
        frameworks.insert("transformers".to_string(), "4.44.2".to_string());
        frameworks.insert("peft".to_string(), "0.12.0".to_string());
        frameworks.insert("trl".to_string(), "0.9.6".to_string());
        RuntimeManifest {
            runtime_id: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            ),
            workload: Workload::Finetune,
            hardware_profile: HardwareProfile {
                gpu_model: "NVIDIA A100-SXM4-40GB".to_string(),
                gpu_count: 1,
                vram_gb: 40.0,
                driver_version: "550.90.07".to_string(),
                cuda_max_supported: "12.4".to_string(),
            },
            python: "3.12.13".to_string(),
            cuda_runtime: "12.4.1".to_string(),
            torch: "2.4.0".to_string(),
            frameworks,
            precision: WeightPrecision::Bf16,
            training_strategy: TrainingStrategy::Qlora,
            base_image_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            validation_level: ValidationLevel::Resolved,
        }
    }

    #[test]
    fn runtime_id_excludes_runtime_id_field() {
        // Setting runtime_id to a different value must not change the hash.
        let mut a = sample();
        let mut b = sample();
        a.runtime_id = None;
        b.runtime_id = Some(
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string(),
        );
        assert_eq!(runtime_id(&a), runtime_id(&b));
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let m = sample();
        let first = m.canonical_bytes();
        let second = m.canonical_bytes();
        assert_eq!(first, second, "canonical bytes must be stable across calls");
        assert_eq!(runtime_id(&m), runtime_id(&m));
    }

    #[test]
    fn frameworks_key_order_does_not_matter() {
        let mut a = sample();
        let mut b = sample();
        // BTreeMap already sorts, but rebuild in different insertion order to be sure.
        let mut reordered = BTreeMap::new();
        reordered.insert("trl".to_string(), "0.9.6".to_string());
        reordered.insert("transformers".to_string(), "4.44.2".to_string());
        reordered.insert("peft".to_string(), "0.12.0".to_string());
        a.frameworks = reordered;
        b.frameworks = a.frameworks.clone();
        assert_eq!(runtime_id(&a), runtime_id(&b));
    }

    #[test]
    fn known_answer_matches() {
        let m = sample();
        assert_eq!(
            runtime_id(&m),
            KNOWN_RUNTIME_ID,
            "canonical hash changed; if intentional, update KNOWN_RUNTIME_ID and version the contract"
        );
    }

    #[test]
    fn json_and_struct_agree() {
        let json = include_str!("../../../schemas/examples/runtime_manifest.example.json");
        let parsed = runtime_manifest_from_json(json).expect("example json parses");
        assert_eq!(runtime_id(&parsed), runtime_id(&sample()));
    }

    // -- Immutable-resource known-answer tests ------------------------------
    //
    // If a sample or canonicalization scheme changes, update these literals and
    // version the contract before external bindings consume it.

    const KNOWN_ARTIFACT_ID: &str =
        "sha256:25c57c49626124d95c0a6135e71a3c5d7ec78a22736aa884f988ede8ea43c77c";
    const KNOWN_REVISION_ID: &str =
        "sha256:21fdd1007947fc2ab7d8ecea3684d598de3f7ed51bf63df0ee75207fe80a035a";

    fn artifact_sample() -> ArtifactManifest {
        let json = include_str!("../../../schemas/examples/artifact_manifest.example.json");
        serde_json::from_str(json).expect("artifact example json parses")
    }

    fn revision_sample() -> TrainingRevision {
        let json = include_str!("../../../schemas/examples/training_revision.example.json");
        serde_json::from_str(json).expect("training revision example json parses")
    }

    #[test]
    fn artifact_known_answer_matches() {
        assert_eq!(
            artifact_id(&artifact_sample()),
            KNOWN_ARTIFACT_ID,
            "canonical hash changed; if intentional, update KNOWN_ARTIFACT_ID and version the contract"
        );
    }

    #[test]
    fn revision_known_answer_matches() {
        assert_eq!(
            revision_id(&revision_sample()),
            KNOWN_REVISION_ID,
            "canonical hash changed; if intentional, update KNOWN_REVISION_ID and version the contract"
        );
    }

    #[test]
    fn artifact_id_excludes_artifact_id_field() {
        let mut a = artifact_sample();
        let mut b = artifact_sample();
        a.artifact_id = None;
        b.artifact_id = Some("sha256:".to_string() + &"f".repeat(64));
        assert_eq!(artifact_id(&a), artifact_id(&b));
    }

    #[test]
    fn revision_id_excludes_revision_id_field() {
        let mut a = revision_sample();
        let mut b = revision_sample();
        a.revision_id = None;
        b.revision_id = Some("sha256:".to_string() + &"f".repeat(64));
        assert_eq!(revision_id(&a), revision_id(&b));
    }

    #[test]
    fn immutable_ids_are_deterministic() {
        assert_eq!(
            artifact_id(&artifact_sample()),
            artifact_id(&artifact_sample())
        );
        assert_eq!(
            revision_id(&revision_sample()),
            revision_id(&revision_sample())
        );
    }

    // -- RFC 8785 float-battery known-answer -------------------------------
    //
    // A revision whose free-form snapshot contains fractional/exponential
    // floats (`0.00002`, `0.42137624`, `1.7320508075688772`, `1000000.0`).
    // The old hand-rolled canonicalizer emitted inconsistent representations
    // for values such as `0.00002`; RFC 8785 (JCS) fixes the representation.
    const KNOWN_ADVERSARIAL_REVISION_ID: &str =
        "sha256:1e71cfa0560a8a2ca65ff04e09585c4ccd4c9dd549c4682b3531e18e8f5cafd8";

    fn adversarial_revision() -> TrainingRevision {
        let json = r#"{
            "revision_id": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "run_id": "run-adv",
            "reason": "float_battery",
            "input_metrics": {"lr": 0.00002, "loss": 0.42137624, "grad_norm": 1.7320508075688772, "big": 1000000.0},
            "decision_rule": "test.v1",
            "config_before": {},
            "config_after": {},
            "state_retention": {"model": true, "optimizer": true, "scheduler": true, "grad_scaler": false},
            "rolled_back": false,
            "actor": "rule"
        }"#;
        serde_json::from_str(json).expect("adversarial revision parses")
    }

    #[test]
    fn adversarial_float_known_answer_matches() {
        assert_eq!(
            revision_id(&adversarial_revision()),
            KNOWN_ADVERSARIAL_REVISION_ID,
            "RFC 8785 float canonicalization changed; version the contract before updating"
        );
    }
}
