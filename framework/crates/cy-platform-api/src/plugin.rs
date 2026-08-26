//! Canonical Platform plugin capability registry and deterministic resolver.
//!
//! The manifest data model lives in `cy-manifest`, the Platform-owned contract
//! crate. This module only supplies the local registry and resolution policy;
//! it does not install, download, or manage plugin artifacts.

use cy_manifest::{
    CapabilityId, ExecutionMode, PluginIdentity, PluginManifest, PluginRequirement, PluginSetLock,
    PluginSetSpec, PluginVersion, ResolvedCapability,
};
use std::collections::BTreeMap;
use std::fmt;

/// Version of the generic Platform plugin contract.
pub const PLUGIN_CONTRACT_VERSION: &str = "cyrene.plugin.v1";

/// Explicit failures returned by registry validation and resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityResolutionError {
    InvalidManifest(String),
    DuplicateManifest {
        plugin: String,
        version: String,
    },
    NoProvider {
        capability: String,
    },
    InterfaceMismatch {
        capability: String,
        requested: String,
        available: Vec<String>,
    },
    ExecutionModeMismatch {
        capability: String,
        requested: Vec<ExecutionMode>,
    },
}

impl fmt::Display for CapabilityResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(message) => write!(f, "invalid plugin manifest: {message}"),
            Self::DuplicateManifest { plugin, version } => {
                write!(f, "duplicate plugin manifest: {plugin}@{version}")
            }
            Self::NoProvider { capability } => {
                write!(f, "no provider registered for capability {capability}")
            }
            Self::InterfaceMismatch {
                capability,
                requested,
                available,
            } => write!(
                f,
                "no compatible interface for {capability}; requested {requested}, available [{}]",
                available.join(", ")
            ),
            Self::ExecutionModeMismatch {
                capability,
                requested,
            } => write!(
                f,
                "no compatible execution mode for {capability}; requested {:?}",
                requested
            ),
        }
    }
}

impl std::error::Error for CapabilityResolutionError {}

/// In-memory/reference registry for manifests known to the current process.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    manifests: BTreeMap<(String, String), PluginManifest>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one manifest. Identity and release version form the unique
    /// key; replacing an existing manifest is intentionally not implicit.
    pub fn register(&mut self, manifest: PluginManifest) -> Result<(), CapabilityResolutionError> {
        let plugin = PluginIdentity::new(manifest.plugin.id.clone())
            .map_err(CapabilityResolutionError::InvalidManifest)?;
        let version = PluginVersion::new(manifest.plugin.version.clone())
            .map_err(CapabilityResolutionError::InvalidManifest)?;

        for descriptor in &manifest.capability_descriptors {
            if descriptor.id.id.trim().is_empty()
                || descriptor.interface_version.version.trim().is_empty()
                || descriptor.execution_modes.is_empty()
            {
                return Err(CapabilityResolutionError::InvalidManifest(format!(
                    "{} contains an incomplete capability descriptor",
                    plugin.id
                )));
            }
        }

        let key = (plugin.id.clone(), version.version.clone());
        if self.manifests.contains_key(&key) {
            return Err(CapabilityResolutionError::DuplicateManifest {
                plugin: plugin.id,
                version: version.version,
            });
        }
        self.manifests.insert(key, manifest);
        Ok(())
    }

    /// Return matching manifests in deterministic identity/version order.
    pub fn providers(&self, capability: &CapabilityId) -> Vec<PluginManifest> {
        self.manifests
            .values()
            .filter(|manifest| {
                manifest
                    .capability_descriptors
                    .iter()
                    .any(|descriptor| descriptor.id == *capability)
            })
            .cloned()
            .collect()
    }

    pub fn resolver(&self) -> CapabilityResolver<'_> {
        CapabilityResolver { registry: self }
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }
}

/// Deterministic resolver over a reference registry.
pub struct CapabilityResolver<'a> {
    registry: &'a CapabilityRegistry,
}

impl<'a> CapabilityResolver<'a> {
    pub fn new(registry: &'a CapabilityRegistry) -> Self {
        Self { registry }
    }

    pub fn resolve(
        &self,
        requirement: &PluginRequirement,
    ) -> Result<ResolvedCapability, CapabilityResolutionError> {
        self.resolve_with_modes(requirement, &[])
    }

    pub fn resolve_plugin_set(
        &self,
        spec: &PluginSetSpec,
    ) -> Result<PluginSetLock, CapabilityResolutionError> {
        let mut entries = Vec::with_capacity(spec.requirements.len());
        for requirement in &spec.requirements {
            entries.push(self.resolve_with_modes(requirement, &spec.allowed_execution_modes)?);
        }
        PluginSetLock::new(entries)
            .with_digest()
            .map_err(CapabilityResolutionError::InvalidManifest)
    }

    fn resolve_with_modes(
        &self,
        requirement: &PluginRequirement,
        globally_allowed_modes: &[ExecutionMode],
    ) -> Result<ResolvedCapability, CapabilityResolutionError> {
        let manifests = self.registry.providers(&requirement.capability);
        if manifests.is_empty() {
            return Err(CapabilityResolutionError::NoProvider {
                capability: requirement.capability.id.clone(),
            });
        }

        let mut available_interfaces = manifests
            .iter()
            .flat_map(|manifest| {
                manifest
                    .capability_descriptors
                    .iter()
                    .filter(|descriptor| descriptor.id == requirement.capability)
                    .map(|descriptor| descriptor.interface_version.version.clone())
            })
            .collect::<Vec<_>>();
        available_interfaces.sort();
        available_interfaces.dedup();

        let matching_interface = manifests.iter().flat_map(|manifest| {
            manifest
                .capability_descriptors
                .iter()
                .filter(|descriptor| {
                    descriptor.id == requirement.capability
                        && descriptor.interface_version == requirement.interface_version
                })
                .map(move |descriptor| (manifest, descriptor))
        });

        let allowed_modes = allowed_modes(requirement, globally_allowed_modes);
        let mut candidates = matching_interface
            .flat_map(|(manifest, descriptor)| {
                allowed_modes
                    .iter()
                    .copied()
                    .filter(|mode| descriptor.supports_mode(*mode))
                    .map(move |mode| (manifest, descriptor, mode))
            })
            .collect::<Vec<_>>();

        if candidates.is_empty() {
            if available_interfaces
                .iter()
                .all(|version| version != &requirement.interface_version.version)
            {
                return Err(CapabilityResolutionError::InterfaceMismatch {
                    capability: requirement.capability.id.clone(),
                    requested: requirement.interface_version.version.clone(),
                    available: available_interfaces,
                });
            }
            return Err(CapabilityResolutionError::ExecutionModeMismatch {
                capability: requirement.capability.id.clone(),
                requested: allowed_modes,
            });
        }

        candidates.sort_by(
            |(left_manifest, _, left_mode), (right_manifest, _, right_mode)| {
                (
                    &left_manifest.plugin.id,
                    &left_manifest.plugin.version,
                    *left_mode,
                )
                    .cmp(&(
                        &right_manifest.plugin.id,
                        &right_manifest.plugin.version,
                        *right_mode,
                    ))
            },
        );

        let (manifest, descriptor, mode) = candidates[0];
        let plugin = PluginIdentity::new(manifest.plugin.id.clone())
            .map_err(CapabilityResolutionError::InvalidManifest)?;
        let plugin_version = PluginVersion::new(manifest.plugin.version.clone())
            .map_err(CapabilityResolutionError::InvalidManifest)?;

        Ok(ResolvedCapability {
            plugin,
            plugin_version,
            capability: descriptor.id.clone(),
            interface_version: descriptor.interface_version.clone(),
            execution_mode: mode,
            artifact: manifest.artifact.clone(),
            compatibility_evidence: vec![
                "capability-id-exact".to_string(),
                "interface-version-exact".to_string(),
                format!("execution-mode:{}", mode.as_str()),
            ],
        })
    }
}

fn allowed_modes(
    requirement: &PluginRequirement,
    globally_allowed_modes: &[ExecutionMode],
) -> Vec<ExecutionMode> {
    let requested = if requirement.execution_modes.is_empty() {
        ExecutionMode::ALL.to_vec()
    } else {
        requirement.execution_modes.clone()
    };
    let mut modes = requested
        .into_iter()
        .filter(|mode| globally_allowed_modes.is_empty() || globally_allowed_modes.contains(mode))
        .collect::<Vec<_>>();
    modes.sort();
    modes.dedup();
    modes
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_manifest::{
        CapabilityDescriptor, CapabilityInterfaceVersion, Edition, PluginCapabilitiesManifest,
        PluginDependencies, PluginMetadata, PluginPackage, RestartPolicy,
    };

    fn manifest(
        id: &str,
        version: &str,
        capability: &str,
        interface_version: &str,
        modes: &[ExecutionMode],
    ) -> PluginManifest {
        PluginManifest {
            plugin: PluginMetadata {
                id: id.to_string(),
                name: id.to_string(),
                version: version.to_string(),
                api_version: "1.0".to_string(),
                kind: "service".to_string(),
                edition: Edition::Community,
                runtime: None,
                license_gate: false,
                entrypoint: None,
                description: None,
                author: None,
                license: None,
                source_target: None,
                status: None,
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
            package: Some(PluginPackage {
                sha256: Some("legacy-package-digest".to_string()),
                signature: None,
                target_os: Vec::new(),
                target_arch: Vec::new(),
            }),
            capability_descriptors: vec![
                CapabilityDescriptor::new(
                    CapabilityId::new(capability).unwrap(),
                    CapabilityInterfaceVersion::new(interface_version).unwrap(),
                    modes.iter().copied(),
                )
                .unwrap(),
            ],
            artifact: Some(cy_manifest::PluginArtifactRef {
                uri: format!("local://{id}/{version}"),
                digest: Some(format!("sha256:{id}-{version}")),
            }),
        }
    }

    fn requirement(
        capability: &str,
        interface_version: &str,
        execution_modes: &[ExecutionMode],
    ) -> PluginRequirement {
        PluginRequirement {
            capability: CapabilityId::new(capability).unwrap(),
            interface_version: CapabilityInterfaceVersion::new(interface_version).unwrap(),
            execution_modes: execution_modes.to_vec(),
        }
    }

    #[test]
    fn registry_queries_registered_capability_providers() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(manifest(
                "cyrene.training.reference",
                "1.0.0",
                "training.engine.v1",
                "1",
                &[ExecutionMode::Worker],
            ))
            .unwrap();

        let providers = registry.providers(&CapabilityId::new("training.engine.v1").unwrap());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].plugin.id, "cyrene.training.reference");
    }

    #[test]
    fn resolver_is_deterministic_and_prefers_explicit_mode_order() {
        let first = manifest(
            "cyrene.zeta",
            "1.0.0",
            "serving.engine.v1",
            "1",
            &[ExecutionMode::Service],
        );
        let second = manifest(
            "cyrene.alpha",
            "1.0.0",
            "serving.engine.v1",
            "1",
            &[ExecutionMode::Inline, ExecutionMode::Worker],
        );
        let requirement = requirement(
            "serving.engine.v1",
            "1",
            &[ExecutionMode::Worker, ExecutionMode::Inline],
        );

        let mut left = CapabilityRegistry::new();
        left.register(first.clone()).unwrap();
        left.register(second.clone()).unwrap();
        let mut right = CapabilityRegistry::new();
        right.register(second).unwrap();
        right.register(first).unwrap();

        let left_result = left.resolver().resolve(&requirement).unwrap();
        let right_result = right.resolver().resolve(&requirement).unwrap();
        assert_eq!(left_result, right_result);
        assert_eq!(left_result.plugin.id, "cyrene.alpha");
        assert_eq!(left_result.execution_mode, ExecutionMode::Inline);
        assert_eq!(
            left_result.artifact.unwrap().digest.as_deref(),
            Some("sha256:cyrene.alpha-1.0.0")
        );
    }

    #[test]
    fn resolver_reports_interface_mismatch_explicitly() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(manifest(
                "cyrene.training.reference",
                "1.0.0",
                "training.engine.v1",
                "1",
                &[ExecutionMode::Worker],
            ))
            .unwrap();

        let error = registry
            .resolver()
            .resolve(&requirement("training.engine.v1", "2", &[]))
            .unwrap_err();
        assert!(matches!(
            error,
            CapabilityResolutionError::InterfaceMismatch { .. }
        ));
    }

    #[test]
    fn plugin_set_lock_is_sorted_and_hashed_from_exact_entries() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(manifest(
                "cyrene.training.reference",
                "1.0.0",
                "training.engine.v1",
                "1",
                &[ExecutionMode::Worker],
            ))
            .unwrap();
        registry
            .register(manifest(
                "cyrene.serving.reference",
                "1.0.0",
                "serving.engine.v1",
                "1",
                &[ExecutionMode::Service],
            ))
            .unwrap();

        let spec = PluginSetSpec {
            requirements: vec![
                requirement("serving.engine.v1", "1", &[]),
                requirement("training.engine.v1", "1", &[]),
            ],
            allowed_execution_modes: vec![ExecutionMode::Worker, ExecutionMode::Service],
        };
        let lock = registry.resolver().resolve_plugin_set(&spec).unwrap();
        assert_eq!(lock.entries[0].capability.id, "serving.engine.v1");
        assert_eq!(lock.entries[1].capability.id, "training.engine.v1");
        assert!(
            lock.digest
                .as_deref()
                .is_some_and(|digest| { digest.starts_with("sha256:") && digest.len() == 71 })
        );
        assert_eq!(lock.entries[0].execution_mode, ExecutionMode::Service);
        assert_eq!(lock.entries[1].execution_mode, ExecutionMode::Worker);
    }
}
