// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/src/bin/cyrene-capability-resolver.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CLI entry point for capability manifest normalization and resolution.
//!
//! Capability manifest 标准化与解析 CLI 入口。
//!
//! # STDOUT CONTRACT
//!
//! This binary writes a **single JSON object and nothing else** to stdout.
//! Any diagnostic or log line added here will be parsed as protocol output by
//! downstream consumers, which read stdout first and only fall back to stderr.
//!
//! - success → `{"resolutions": [...]}` and exit 0
//! - failure → `{"error": "<message>"}` and exit 2
//!
//! Therefore:
//! - Keep `println!` for exactly those two protocol objects.
//! - All diagnostics, warnings, and progress output MUST use `eprintln!`.
//! - Never emit free text (including tracing/log output) to stdout.
use std::io::{self, Read};

use cy_platform_api::{
    CapabilityId, CapabilityInterfaceVersion, CapabilityRegistry, CapabilityResolver,
    ExecutionMode, PluginManifest, PluginRequirement, normalize_repository_manifest,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct ResolverRequest {
    /// Accepts the existing Platform manifest shape and the
    /// repository-facing plugin manifest shape. Both normalize to
    /// the same Platform `PluginManifest` before registry registration.
    manifests: Vec<serde_json::Value>,
    requirements: Vec<RequirementWire>,
}

#[derive(Debug, Deserialize)]
struct RequirementWire {
    capability: String,
    interface_version: String,
    #[serde(default)]
    execution_modes: Vec<ExecutionMode>,
}

fn resolve(request: ResolverRequest) -> Result<serde_json::Value, String> {
    let mut registry = CapabilityRegistry::new();
    for value in request.manifests {
        let manifest = normalize_manifest(value)?;
        registry
            .register(manifest)
            .map_err(|error| error.to_string())?;
    }

    let resolver = CapabilityResolver::new(&registry);
    let mut resolutions = Vec::with_capacity(request.requirements.len());
    for wire in request.requirements {
        let requirement = PluginRequirement {
            capability: CapabilityId::new(wire.capability).map_err(|error| error.to_string())?,
            interface_version: CapabilityInterfaceVersion::new(wire.interface_version)
                .map_err(|error| error.to_string())?,
            execution_modes: wire.execution_modes,
        };
        resolutions.push(
            resolver
                .resolve(&requirement)
                .map_err(|error| error.to_string())?,
        );
    }

    Ok(json!({"resolutions": resolutions}))
}

fn normalize_manifest(value: serde_json::Value) -> Result<PluginManifest, String> {
    if value.get("plugin").is_some() {
        serde_json::from_value(value).map_err(|error| format!("invalid Platform manifest: {error}"))
    } else {
        normalize_repository_manifest(value)
    }
}

fn main() {
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("failed to read resolver request: {error}");
        std::process::exit(2);
    }

    let result = serde_json::from_str::<ResolverRequest>(&input)
        .map_err(|error| error.to_string())
        .and_then(resolve);
    match result {
        Ok(output) => println!("{output}"),
        Err(error) => {
            println!("{}", json!({"error": error}));
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_accepts_the_repository_manifest_shape() {
        let request = ResolverRequest {
            manifests: vec![json!({
                "schemaVersion": 1,
                "id": "example.worker",
                "name": "Example Worker",
                "version": "0.1.0",
                "kind": "capability-plugin",
                "capabilities": ["example.transform.v1"],
                "methods": [
                    {"name": "inspect", "interfaceVersion": "1", "executionMode": "worker"},
                    {"name": "transform", "interfaceVersion": "1", "executionMode": "worker"}
                ],
                "runtime": {"language": "python", "entrypoint": "example_worker:Worker"}
            })],
            requirements: vec![RequirementWire {
                capability: "example.transform.v1".to_string(),
                interface_version: "1".to_string(),
                execution_modes: vec![ExecutionMode::Worker],
            }],
        };
        let result = resolve(request).unwrap();
        assert_eq!(result["resolutions"][0]["plugin"]["id"], "example.worker");
        assert_eq!(result["resolutions"][0]["execution_mode"], "WORKER");
    }
}
