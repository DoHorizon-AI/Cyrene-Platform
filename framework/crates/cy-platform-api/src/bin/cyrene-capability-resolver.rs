use std::io::{self, Read};

use cy_platform_api::{
    CapabilityId, CapabilityInterfaceVersion, CapabilityRegistry, CapabilityResolver,
    ExecutionMode, PluginManifest, PluginRequirement,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct ResolverRequest {
    manifests: Vec<PluginManifest>,
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
    for manifest in request.manifests {
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
