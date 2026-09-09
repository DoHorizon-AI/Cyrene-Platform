// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-platform-api/src/plugin.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
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
    InvalidBinding(String),
    DuplicateManifest {
        plugin: String,
        version: String,
    },
    DuplicateBinding {
        binding: String,
    },
    UnknownBinding {
        binding: String,
    },
    BindingProviderUnavailable {
        binding: String,
        plugin: String,
        version: String,
    },
    BindingCapabilityMismatch {
        binding: String,
        capability: String,
        reason: String,
    },
    AmbiguousBinding {
        capability: String,
        bindings: Vec<String>,
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
            Self::InvalidBinding(message) => write!(f, "invalid capability binding: {message}"),
            Self::DuplicateManifest { plugin, version } => {
                write!(f, "duplicate plugin manifest: {plugin}@{version}")
            }
            Self::DuplicateBinding { binding } => {
                write!(f, "duplicate configured capability binding: {binding}")
            }
            Self::UnknownBinding { binding } => {
                write!(f, "unknown configured capability binding: {binding}")
            }
            Self::BindingProviderUnavailable {
                binding,
                plugin,
                version,
            } => write!(
                f,
                "configured capability binding {binding} points to unavailable provider {plugin}@{version}"
            ),
            Self::BindingCapabilityMismatch {
                binding,
                capability,
                reason,
            } => write!(
                f,
                "configured capability binding {binding} does not expose capability {capability}: {reason}"
            ),
            Self::AmbiguousBinding {
                capability,
                bindings,
            } => write!(
                f,
                "ambiguous configured capability selection for {capability}; specify binding_id; matching bindings [{}]",
                bindings.join(", ")
            ),
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

/// Stable identity for one configured capability instance.
///
/// The binding points at an immutable plugin release, while its `id` remains
/// stable when the worker process or runtime generation is replaced. Runtime
/// activation settings are supplied by the execution service separately so
/// this identity remains independent from process and executable details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityBinding {
    pub id: String,
    pub plugin: PluginIdentity,
    pub plugin_version: PluginVersion,
}

impl CapabilityBinding {
    pub fn new(
        id: impl Into<String>,
        plugin_id: impl Into<String>,
        plugin_version: impl Into<String>,
    ) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("capability binding identity must not be empty".to_string());
        }
        Ok(Self {
            id,
            plugin: PluginIdentity::new(plugin_id)?,
            plugin_version: PluginVersion::new(plugin_version)?,
        })
    }
}

/// A resolved capability together with the stable configured identity, when
/// resolution selected one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCapabilityTarget {
    pub capability: ResolvedCapability,
    pub binding_id: Option<String>,
}

/// In-memory/reference registry for manifests known to the current process.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    manifests: BTreeMap<(String, String), PluginManifest>,
    bindings: BTreeMap<String, CapabilityBinding>,
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

    /// Register one configured capability binding against an already-known
    /// plugin release. The registry remains the single authority for both
    /// manifest and configured-instance lookup.
    pub fn register_binding(
        &mut self,
        binding: CapabilityBinding,
    ) -> Result<(), CapabilityResolutionError> {
        if binding.id.trim().is_empty() {
            return Err(CapabilityResolutionError::InvalidBinding(
                "capability binding identity must not be empty".to_string(),
            ));
        }
        let key = (
            binding.plugin.id.clone(),
            binding.plugin_version.version.clone(),
        );
        if !self.manifests.contains_key(&key) {
            return Err(CapabilityResolutionError::BindingProviderUnavailable {
                binding: binding.id,
                plugin: key.0,
                version: key.1,
            });
        }
        if self.bindings.contains_key(&binding.id) {
            return Err(CapabilityResolutionError::DuplicateBinding {
                binding: binding.id,
            });
        }
        self.bindings.insert(binding.id.clone(), binding);
        Ok(())
    }

    /// Return one exact configured binding by its stable identity.
    pub fn binding(&self, binding_id: &str) -> Option<&CapabilityBinding> {
        self.bindings.get(binding_id)
    }

    pub fn binding_count(&self) -> usize {
        self.bindings.len()
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

    /// Return one exact registered manifest by immutable provider identity.
    ///
    /// Resolution remains the only selection authority; this lookup is used
    /// only after a resolved provider has been selected so the execution
    /// service can pass the same canonical manifest to the worker activator.
    pub fn manifest_for(&self, plugin_id: &str, plugin_version: &str) -> Option<PluginManifest> {
        self.manifests
            .get(&(plugin_id.to_string(), plugin_version.to_string()))
            .cloned()
    }

    /// Validate a manifest against an exact capability interface and its
    /// execution constraints without selecting a provider.
    pub fn validate_interface(
        &self,
        manifest: &PluginManifest,
        requirement: &PluginRequirement,
    ) -> Result<(), CapabilityResolutionError> {
        let descriptors = manifest
            .capability_descriptors
            .iter()
            .filter(|descriptor| descriptor.id == requirement.capability)
            .collect::<Vec<_>>();
        if descriptors.is_empty() {
            return Err(CapabilityResolutionError::NoProvider {
                capability: requirement.capability.id.clone(),
            });
        }

        let matching_interface = descriptors
            .iter()
            .filter(|descriptor| descriptor.interface_version == requirement.interface_version)
            .any(|descriptor| {
                allowed_modes(requirement, &[])
                    .iter()
                    .any(|mode| descriptor.supports_mode(*mode))
            });
        if matching_interface {
            return Ok(());
        }

        let mut available = descriptors
            .iter()
            .map(|descriptor| descriptor.interface_version.version.clone())
            .collect::<Vec<_>>();
        available.sort();
        available.dedup();
        if available
            .iter()
            .all(|version| version != &requirement.interface_version.version)
        {
            return Err(CapabilityResolutionError::InterfaceMismatch {
                capability: requirement.capability.id.clone(),
                requested: requirement.interface_version.version.clone(),
                available,
            });
        }
        Err(CapabilityResolutionError::ExecutionModeMismatch {
            capability: requirement.capability.id.clone(),
            requested: allowed_modes(requirement, &[]),
        })
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

    // ════════════════════════════════════════════════════════════════════════
    // 🔧 FUNCTION: CapabilityResolver::resolve
    //
    //   Resolves one typed capability requirement against the registered
    //   manifests and returns a deterministic compatibility result.
    //
    //   将一个强类型能力需求与已注册 Manifest 进行匹配，返回确定性的兼容结果。
    //   具体匹配由硬件、精度、量化、流式能力和接口约束共同决定。
    // ════════════════════════════════════════════════════════════════════════
    pub fn resolve(
        &self,
        requirement: &PluginRequirement,
    ) -> Result<ResolvedCapability, CapabilityResolutionError> {
        self.resolve_with_modes(requirement, &[])
    }

    /// Resolve an exact configured binding when requested. Without a target,
    /// configured bindings are considered only when they expose the requested
    /// capability/interface/mode: one match is selected, multiple matches are
    /// rejected, and zero matches retain the legacy provider resolution path.
    pub fn resolve_target(
        &self,
        requirement: &PluginRequirement,
        binding_id: Option<&str>,
    ) -> Result<ResolvedCapabilityTarget, CapabilityResolutionError> {
        if let Some(binding_id) = binding_id.map(str::trim).filter(|id| !id.is_empty()) {
            let binding = self.registry.bindings.get(binding_id).ok_or_else(|| {
                CapabilityResolutionError::UnknownBinding {
                    binding: binding_id.to_string(),
                }
            })?;
            let manifest = self
                .registry
                .manifest_for(&binding.plugin.id, &binding.plugin_version.version)
                .ok_or_else(|| CapabilityResolutionError::BindingProviderUnavailable {
                    binding: binding.id.clone(),
                    plugin: binding.plugin.id.clone(),
                    version: binding.plugin_version.version.clone(),
                })?;
            let capability =
                resolve_manifest_with_modes(&manifest, requirement, &[]).map_err(|error| {
                    CapabilityResolutionError::BindingCapabilityMismatch {
                        binding: binding.id.clone(),
                        capability: requirement.capability.id.clone(),
                        reason: error.to_string(),
                    }
                })?;
            return Ok(ResolvedCapabilityTarget {
                capability,
                binding_id: Some(binding.id.clone()),
            });
        }

        let mut matches = Vec::new();
        for binding in self.registry.bindings.values() {
            let Some(manifest) = self
                .registry
                .manifest_for(&binding.plugin.id, &binding.plugin_version.version)
            else {
                continue;
            };
            if let Ok(capability) = resolve_manifest_with_modes(&manifest, requirement, &[]) {
                matches.push((binding.id.clone(), capability));
            }
        }
        if matches.len() > 1 {
            return Err(CapabilityResolutionError::AmbiguousBinding {
                capability: requirement.capability.id.clone(),
                bindings: matches.into_iter().map(|(id, _)| id).collect(),
            });
        }
        if let Some((binding_id, capability)) = matches.pop() {
            return Ok(ResolvedCapabilityTarget {
                capability,
                binding_id: Some(binding_id),
            });
        }

        self.resolve(requirement)
            .map(|capability| ResolvedCapabilityTarget {
                capability,
                binding_id: None,
            })
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
            artifact: manifest.artifact_reference(),
            compatibility_evidence: vec![
                "capability-id-exact".to_string(),
                "interface-version-exact".to_string(),
                format!("execution-mode:{}", mode.as_str()),
            ],
        })
    }
}

fn resolve_manifest_with_modes(
    manifest: &PluginManifest,
    requirement: &PluginRequirement,
    globally_allowed_modes: &[ExecutionMode],
) -> Result<ResolvedCapability, CapabilityResolutionError> {
    let descriptors = manifest
        .capability_descriptors
        .iter()
        .filter(|descriptor| descriptor.id == requirement.capability)
        .collect::<Vec<_>>();
    if descriptors.is_empty() {
        return Err(CapabilityResolutionError::NoProvider {
            capability: requirement.capability.id.clone(),
        });
    }

    let mut available_interfaces = descriptors
        .iter()
        .map(|descriptor| descriptor.interface_version.version.clone())
        .collect::<Vec<_>>();
    available_interfaces.sort();
    available_interfaces.dedup();

    let allowed_modes = allowed_modes(requirement, globally_allowed_modes);
    let mut candidates = descriptors
        .iter()
        .flat_map(|descriptor| {
            allowed_modes
                .iter()
                .copied()
                .filter(|mode| {
                    descriptor.interface_version == requirement.interface_version
                        && descriptor.supports_mode(*mode)
                })
                .map(move |mode| (descriptor, mode))
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

    candidates.sort_by_key(|(_, mode)| *mode);
    let (descriptor, mode) = candidates[0];
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
        artifact: manifest.artifact_reference(),
        compatibility_evidence: vec![
            "capability-id-exact".to_string(),
            "interface-version-exact".to_string(),
            format!("execution-mode:{}", mode.as_str()),
        ],
    })
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
    use cy_manifest::{CapabilityDescriptor, CapabilityInterfaceVersion, PluginMetadata};

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
                runtime: None,
                launch: None,
            },
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

    fn binding(id: &str) -> CapabilityBinding {
        CapabilityBinding::new(id, "cyrene.training.reference", "1.0.0").unwrap()
    }

    #[test]
    fn registry_queries_registered_capability_providers() {
        let mut registry = CapabilityRegistry::new();
        let registered_manifest = manifest(
            "cyrene.training.reference",
            "1.0.0",
            "training.engine.v1",
            "1",
            &[ExecutionMode::Worker],
        );
        registry.register(registered_manifest.clone()).unwrap();

        let providers = registry.providers(&CapabilityId::new("training.engine.v1").unwrap());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].plugin.id, "cyrene.training.reference");
        assert_eq!(
            registry.manifest_for("cyrene.training.reference", "1.0.0"),
            Some(registered_manifest)
        );
        assert!(
            registry
                .manifest_for("cyrene.training.reference", "2.0.0")
                .is_none()
        );
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
        let registered = registry
            .providers(&CapabilityId::new("training.engine.v1").unwrap())
            .pop()
            .unwrap();
        assert!(matches!(
            registry.validate_interface(&registered, &requirement("training.engine.v1", "2", &[])),
            Err(CapabilityResolutionError::InterfaceMismatch { .. })
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

    #[test]
    fn configured_binding_resolution_is_explicit_and_stable() {
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
        registry.register_binding(binding("training-main")).unwrap();

        let target = registry
            .resolver()
            .resolve_target(
                &requirement("training.engine.v1", "1", &[]),
                Some("training-main"),
            )
            .unwrap();
        assert_eq!(target.binding_id.as_deref(), Some("training-main"));
        assert_eq!(target.capability.plugin.id, "cyrene.training.reference");
        assert_eq!(
            registry.binding("training-main").unwrap().id,
            "training-main"
        );

        let implicit = registry
            .resolver()
            .resolve_target(&requirement("training.engine.v1", "1", &[]), None)
            .unwrap();
        assert_eq!(implicit.binding_id.as_deref(), Some("training-main"));
    }

    #[test]
    fn configured_binding_implicit_selection_rejects_ambiguity() {
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
        registry.register_binding(binding("training-main")).unwrap();
        registry
            .register_binding(binding("training-secondary"))
            .unwrap();

        let error = registry
            .resolver()
            .resolve_target(&requirement("training.engine.v1", "1", &[]), None)
            .unwrap_err();
        assert_eq!(
            error,
            CapabilityResolutionError::AmbiguousBinding {
                capability: "training.engine.v1".to_string(),
                bindings: vec![
                    "training-main".to_string(),
                    "training-secondary".to_string()
                ]
            }
        );
    }

    #[test]
    fn configured_binding_reports_unknown_and_capability_mismatch() {
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
        registry.register_binding(binding("training-main")).unwrap();

        assert!(matches!(
            registry.resolver().resolve_target(
                &requirement("training.engine.v1", "1", &[]),
                Some("missing")
            ),
            Err(CapabilityResolutionError::UnknownBinding { binding }) if binding == "missing"
        ));
        assert!(matches!(
            registry.resolver().resolve_target(
                &requirement("storage.engine.v1", "1", &[]),
                Some("training-main")
            ),
            Err(CapabilityResolutionError::BindingCapabilityMismatch { binding, capability, .. })
                if binding == "training-main" && capability == "storage.engine.v1"
        ));
    }
}
