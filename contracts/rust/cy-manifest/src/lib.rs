//! Core CYRENE manifest types and deterministic canonical hashing.
//!
//! The JSON Schemas under `schemas/manifests/` are the single source of truth.
//! The canonicalization scheme in `schemas/CANONICALIZATION.md` is implemented
//! here as the core reference for `canonical_bytes` and content identifiers.

mod canonical;
mod manifest;

pub use canonical::{canonical_sha256_hex, canonicalize};
pub use manifest::*;

use serde::Serialize;
use serde_json::Value;

/// A manifest that can be canonicalized and hashed.
pub trait Manifest: Serialize {
    /// The JSON value tree used as the hash preimage.
    ///
    /// For most manifests this is the manifest serialized to JSON. For
    /// [`RuntimeManifest`] the `runtime_id` field is removed (it is an output,
    /// never part of its own preimage).
    fn canonical_value(&self) -> Value {
        serde_json::to_value(self).expect("manifest is serializable to JSON")
    }

    /// Canonical byte serialization per `schemas/CANONICALIZATION.md`.
    fn canonical_bytes(&self) -> Vec<u8> {
        canonical::canonicalize(&self.canonical_value())
    }

    /// Lowercase hex SHA-256 of [`Manifest::canonical_bytes`].
    fn canonical_sha256_hex(&self) -> String {
        canonical::canonical_sha256_hex(&self.canonical_value())
    }
}

impl Manifest for HardwareManifest {}
impl Manifest for ModelManifest {}
impl Manifest for WorkloadRequest {}
// WhyReport and ValidationResult are not immutable resources and carry no
// content id, but a canonical hash is still available for change detection.
impl Manifest for WhyReport {}
impl Manifest for ValidationResult {}

/// Build the canonical value for a manifest, removing a single computed id key.
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

/// Compute the `runtime_id` of a [`RuntimeManifest`].
///
/// `runtime_id = "sha256:" + hex(sha256(canonical_bytes(manifest_without_runtime_id)))`.
pub fn runtime_id(manifest: &RuntimeManifest) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// Compute the `revision_id` of a [`TrainingRevision`] (content hash, id excluded).
pub fn revision_id(manifest: &TrainingRevision) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// Compute the `checkpoint_id` of a [`CheckpointMetadata`] (content hash, id excluded).
pub fn checkpoint_id(manifest: &CheckpointMetadata) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// Compute the `artifact_id` of an [`ArtifactManifest`] (content hash, id excluded).
pub fn artifact_id(manifest: &ArtifactManifest) -> String {
    format!("sha256:{}", manifest.canonical_sha256_hex())
}

/// Parse a [`RuntimeManifest`] from a JSON string.
pub fn runtime_manifest_from_json(s: &str) -> Result<RuntimeManifest, serde_json::Error> {
    serde_json::from_str(s)
}

/// Parse a [`RuntimeManifest`] from a YAML string.
pub fn runtime_manifest_from_yaml(s: &str) -> Result<RuntimeManifest, serde_yaml::Error> {
    serde_yaml::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
