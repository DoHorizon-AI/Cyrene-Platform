use std::io::{self, Read};

use cy_platform_api::{
    CapabilityId, CapabilityInterfaceVersion, CapabilityRegistry, CapabilityResolver,
    ExecutionMode, PluginManifest, PluginRequirement, normalize_official_manifest,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct ResolverRequest {
    /// Accepts the existing Platform manifest shape and the single
    /// repository-facing Official Plugins manifest shape.  Both normalize to
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
        normalize_official_manifest(value)
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
    fn resolver_accepts_the_official_manifest_shape() {
        let request = ResolverRequest {
            manifests: vec![json!({
                "schemaVersion": 1,
                "id": "cyrene.tools.media",
                "name": "CYRENE Generic Media Processor",
                "version": "0.1.0",
                "kind": "capability-plugin",
                "capabilities": ["media.processor.v1"],
                "methods": [
                    {"name": "inspect_image", "interfaceVersion": "1", "executionMode": "worker"},
                    {"name": "transform_image", "interfaceVersion": "1", "executionMode": "worker"}
                ],
                "runtime": {"language": "python", "entrypoint": "media_processor:MediaProcessor"}
            })],
            requirements: vec![RequirementWire {
                capability: "media.processor.v1".to_string(),
                interface_version: "1".to_string(),
                execution_modes: vec![ExecutionMode::Worker],
            }],
        };
        let result = resolve(request).unwrap();
        assert_eq!(
            result["resolutions"][0]["plugin"]["id"],
            "cyrene.tools.media"
        );
        assert_eq!(result["resolutions"][0]["execution_mode"], "WORKER");
    }
}
