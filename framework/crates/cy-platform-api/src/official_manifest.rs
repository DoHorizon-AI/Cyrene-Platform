// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/src/official_manifest.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Adapter for the single canonical Official Plugins manifest authority.
//!
//! Official Plugins intentionally keeps its repository-facing
//! `plugin.manifest.json` shape.  The Platform resolver consumes the same file
//! through this loss-aware normalization step; this module does not introduce
//! a second manifest or a second capability registry.

use cy_manifest::{
    CapabilityDescriptor, CapabilityId, CapabilityInterfaceVersion, Edition, ExecutionMode,
    PluginCapabilitiesManifest, PluginDependencies, PluginManifest, PluginMetadata, RestartPolicy,
    Runtime,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The repository-facing Official Plugins manifest shape used by
/// `plugin.manifest.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPluginManifest {
    schema_version: u32,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    kind: String,
    capabilities: Vec<String>,
    #[serde(default)]
    methods: Vec<OfficialPluginMethod>,
    runtime: OfficialPluginRuntime,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfficialPluginMethod {
    name: String,
    execution_mode: String,
    interface_version: Option<String>,
    #[serde(default)]
    capability: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfficialPluginRuntime {
    language: String,
    #[serde(default)]
    entrypoint: Option<String>,
}

impl OfficialPluginManifest {
    /// Normalize the Official Plugins manifest into the Platform resolver's
    /// existing canonical Rust model.
    pub fn into_platform_manifest(self) -> Result<PluginManifest, String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported Official Plugins manifest schemaVersion: {}",
                self.schema_version
            ));
        }

        let plugin_id = self.id.trim().to_string();
        let plugin_version = self.version.trim().to_string();
        if plugin_id.is_empty() || plugin_version.is_empty() || self.name.trim().is_empty() {
            return Err("Official Plugins manifest id, name, and version must be non-empty".into());
        }
        CapabilityId::new(plugin_id.clone())?;
        cy_manifest::PluginVersion::new(plugin_version.clone())?;

        if self.capabilities.is_empty() {
            return Err("Official Plugins manifest must declare at least one capability".into());
        }
        let declared_capabilities: BTreeSet<String> = self
            .capabilities
            .into_iter()
            .map(|capability| capability.trim().to_string())
            .collect();
        if declared_capabilities.iter().any(String::is_empty) {
            return Err("Official Plugins capability IDs must be non-empty".into());
        }
        for capability in &declared_capabilities {
            CapabilityId::new(capability.clone())?;
        }

        if self.methods.is_empty() {
            return Err("Official Plugins manifest must declare at least one method".into());
        }

        let mut descriptor_modes: BTreeMap<(String, String), BTreeSet<ExecutionMode>> =
            BTreeMap::new();
        let mut used_capabilities = BTreeSet::new();
        for method in self.methods {
            if method.name.trim().is_empty() {
                return Err("Official Plugins method names must be non-empty".into());
            }
            let has_explicit_capability = method.capability.is_some();
            let capability = method
                .capability
                .unwrap_or_else(|| {
                    declared_capabilities
                        .iter()
                        .next()
                        .cloned()
                        .unwrap_or_default()
                })
                .trim()
                .to_string();
            if declared_capabilities.len() != 1 && !has_explicit_capability {
                return Err(format!(
                    "method `{}` must name its capability when the manifest declares multiple capabilities",
                    method.name
                ));
            }
            if !declared_capabilities.contains(&capability) {
                return Err(format!(
                    "method `{}` references undeclared capability `{capability}`",
                    method.name
                ));
            }
            let interface_version = method
                .interface_version
                .ok_or_else(|| format!("method `{}` must declare interfaceVersion", method.name))?
                .trim()
                .to_string();
            if interface_version.is_empty() {
                return Err(format!(
                    "method `{}` interfaceVersion must be non-empty",
                    method.name
                ));
            }
            CapabilityInterfaceVersion::new(interface_version.clone())?;
            let mode = parse_execution_mode(&method.execution_mode)?;
            used_capabilities.insert(capability.clone());
            descriptor_modes
                .entry((capability, interface_version))
                .or_default()
                .insert(mode);
        }
        if used_capabilities != declared_capabilities {
            return Err(format!(
                "Official Plugins manifest capabilities without methods: {:?}",
                declared_capabilities
                    .difference(&used_capabilities)
                    .collect::<Vec<_>>()
            ));
        }

        let capability_descriptors = descriptor_modes
            .into_iter()
            .map(|((capability, interface_version), modes)| {
                CapabilityDescriptor::new(
                    CapabilityId::new(capability)?,
                    CapabilityInterfaceVersion::new(interface_version)?,
                    modes,
                )
            })
            .collect::<Result<Vec<_>, String>>()?;

        let runtime = parse_runtime(&self.runtime.language)?;
        Ok(PluginManifest {
            plugin: PluginMetadata {
                id: plugin_id,
                name: self.name,
                version: plugin_version,
                api_version: "1.0".to_string(),
                kind: self.kind,
                edition: Edition::Community,
                runtime: Some(runtime),
                license_gate: false,
                entrypoint: self.runtime.entrypoint,
                description: self.description,
                author: None,
                license: None,
                source_target: None,
                status: Some("active".to_string()),
                protocol_version: None,
                scope: None,
                restart_policy: RestartPolicy::Never,
            },
            capabilities: PluginCapabilitiesManifest::default(),
            dependencies: PluginDependencies::default(),
            components: Vec::new(),
            launch: None,
            permissions: None,
            resources: None,
            package: None,
            capability_descriptors,
            artifact: None,
        })
    }
}

fn parse_execution_mode(value: &str) -> Result<ExecutionMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inline" => Ok(ExecutionMode::Inline),
        "worker" => Ok(ExecutionMode::Worker),
        "service" => Ok(ExecutionMode::Service),
        unsupported => Err(format!(
            "Official Plugins execution mode `{unsupported}` is not supported by the Platform resolver"
        )),
    }
}

fn parse_runtime(value: &str) -> Result<Runtime, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "python" => Ok(Runtime::SubprocessPython),
        "java" => Ok(Runtime::SubprocessJvm),
        "service" => Ok(Runtime::Service),
        unsupported => Err(format!(
            "Official Plugins runtime language `{unsupported}` is not supported by the Platform resolver"
        )),
    }
}

/// Parse and normalize one repository-facing Official Plugins manifest value.
pub fn normalize_official_manifest(value: serde_json::Value) -> Result<PluginManifest, String> {
    let manifest = serde_json::from_value::<OfficialPluginManifest>(value)
        .map_err(|error| format!("invalid Official Plugins manifest: {error}"))?;
    manifest.into_platform_manifest()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media_manifest() -> serde_json::Value {
        json!({
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
        })
    }

    #[test]
    fn normalizes_the_repository_manifest_to_the_existing_resolver_model() {
        let manifest = normalize_official_manifest(media_manifest()).unwrap();
        assert_eq!(manifest.plugin.id, "cyrene.tools.media");
        assert_eq!(manifest.plugin.version, "0.1.0");
        assert_eq!(manifest.capability_descriptors.len(), 1);
        assert_eq!(
            manifest.capability_descriptors[0].id.id,
            "media.processor.v1"
        );
        assert_eq!(
            manifest.capability_descriptors[0].interface_version.version,
            "1"
        );
        assert_eq!(
            manifest.capability_descriptors[0].execution_modes,
            vec![ExecutionMode::Worker]
        );
    }

    #[test]
    fn requires_an_explicit_method_interface_version() {
        let mut manifest = media_manifest();
        manifest["methods"][0]
            .as_object_mut()
            .unwrap()
            .remove("interfaceVersion");
        let error = normalize_official_manifest(manifest).unwrap_err();
        assert!(error.contains("interfaceVersion"));
    }

    #[test]
    fn rejects_modes_that_have_no_platform_execution_semantics() {
        let mut manifest = media_manifest();
        manifest["methods"][0]["executionMode"] = json!("job");
        let error = normalize_official_manifest(manifest).unwrap_err();
        assert!(error.contains("not supported"));
    }
}
