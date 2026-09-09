// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Generic Artifact and Plugin control-plane manifest contracts.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：通用 Artifact 与 Plugin 控制面清单契约。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Platform manifest types and deterministic RFC 8785 hashing.
//!
//! Product model, training, serving, validation, and lineage records live in
//! their owning Product or Plugin repository. This crate contains only the
//! stable control-plane projections shared by independent consumers.

mod canonical;
mod manifest;

pub use canonical::{canonical_sha256_hex, canonicalize};
pub use manifest::*;

use serde::Serialize;
use serde_json::Value;

/// Deterministic RFC 8785 serialization and hashing for a manifest value.
pub trait Manifest: Serialize {
    fn canonical_value(&self) -> Value {
        serde_json::to_value(self).expect("manifest is serializable to JSON")
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        canonicalize(&self.canonical_value())
    }

    fn canonical_sha256_hex(&self) -> String {
        canonical_sha256_hex(&self.canonical_value())
    }
}

impl Manifest for PortableDirectoryManifest {}

/// Compute the content identity of a portable directory manifest.
pub fn portable_directory_id(manifest: &PortableDirectoryManifest) -> String {
    manifest.computed_digest()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_directory_fixture_passes_schema_and_rust_validation() {
        let value: Value = serde_json::from_str(include_str!(
            "../../../schemas/examples/portable_directory_manifest.example.json"
        ))
        .expect("portable directory fixture must be valid JSON");
        let schema: Value = serde_json::from_str(include_str!(
            "../../../schemas/manifests/portable_directory_manifest.schema.json"
        ))
        .expect("portable directory schema must be valid JSON");
        let compiled = jsonschema::JSONSchema::compile(&schema)
            .expect("portable directory schema must compile");
        let errors: Vec<String> = compiled
            .validate(&value)
            .err()
            .into_iter()
            .flat_map(|items| items.map(|error| error.to_string()))
            .collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");

        let manifest: PortableDirectoryManifest =
            serde_json::from_value(value).expect("portable directory fixture must parse");
        manifest
            .validate()
            .expect("portable directory fixture must pass semantic validation");
    }
}
