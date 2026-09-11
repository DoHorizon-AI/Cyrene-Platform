// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/plugin.rs
// ║ Module: CYRENE Platform
// ║ Role: Generic Plugin discovery, compatibility, and launch metadata.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：通用 Plugin 发现、兼容性与启动元数据。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Generic Plugin control-plane projections.
//!
//! Capability payloads, methods, Product state, and language-specific adapters
//! remain in the Plugin or Product repository. Platform sees only identity,
//! compatibility descriptors, process launch metadata, and opaque endpoints.

use serde::{Deserialize, Serialize};

/// Runtime placement needed by the Platform resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    Service,
}

/// Package-relative process launch command.
///
/// The executable and arguments belong to the package. Platform appends the
/// generic capability/interface/listen flags used by the readiness handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginLaunch {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Minimal manifest metadata required by Platform lifecycle and resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime: Option<Runtime>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub launch: Option<PluginLaunch>,
}

/// Platform-internal normalized projection of a repository-owned manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub plugin: PluginMetadata,
    #[serde(default)]
    pub capability_descriptors: Vec<CapabilityDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<PluginArtifactRef>,
}

/// Stable identity of a plugin, independent of its release version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIdentity {
    pub id: String,
}

impl PluginIdentity {
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("plugin identity must not be empty".to_string());
        }
        Ok(Self { id })
    }
}

/// Published plugin version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginVersion {
    pub version: String,
}

impl PluginVersion {
    pub fn new(version: impl Into<String>) -> Result<Self, String> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err("plugin version must not be empty".to_string());
        }
        Ok(Self { version })
    }
}

/// Opaque capability identity owned by a versioned Plugin contract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityId {
    pub id: String,
}

impl CapabilityId {
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("capability id must not be empty".to_string());
        }
        Ok(Self { id })
    }
}

/// Version of the callable capability interface, separate from Plugin release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInterfaceVersion {
    pub version: String,
}

impl CapabilityInterfaceVersion {
    pub fn new(version: impl Into<String>) -> Result<Self, String> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err("capability interface version must not be empty".to_string());
        }
        Ok(Self { version })
    }
}

/// Reference to a materialized Plugin artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginArtifactRef {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<String>,
}

/// Supported execution placement for a resolved capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ExecutionMode {
    Inline,
    Worker,
    Service,
}

impl ExecutionMode {
    pub const ALL: [Self; 3] = [Self::Inline, Self::Worker, Self::Service];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "INLINE",
            Self::Worker => "WORKER",
            Self::Service => "SERVICE",
        }
    }
}

/// Matchable capability declaration from a repository-owned manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    #[serde(default)]
    pub execution_modes: Vec<ExecutionMode>,
}

impl CapabilityDescriptor {
    pub fn new(
        id: CapabilityId,
        interface_version: CapabilityInterfaceVersion,
        execution_modes: impl IntoIterator<Item = ExecutionMode>,
    ) -> Result<Self, String> {
        let mut modes: Vec<_> = execution_modes.into_iter().collect();
        modes.sort();
        modes.dedup();
        if modes.is_empty() {
            return Err("capability descriptor must declare an execution mode".to_string());
        }
        Ok(Self {
            id,
            interface_version,
            execution_modes: modes,
        })
    }

    pub fn supports_mode(&self, mode: ExecutionMode) -> bool {
        self.execution_modes.contains(&mode)
    }
}

/// Capability requirement used by a PluginSet and service adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRequirement {
    pub capability: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    #[serde(default)]
    pub execution_modes: Vec<ExecutionMode>,
}

pub type CapabilityRequirement = PluginRequirement;

/// Logical capability set; it does not imply one environment or process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetSpec {
    #[serde(default)]
    pub requirements: Vec<PluginRequirement>,
    #[serde(default)]
    pub allowed_execution_modes: Vec<ExecutionMode>,
}

/// Deterministic compatibility resolution result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCapability {
    pub plugin: PluginIdentity,
    pub plugin_version: PluginVersion,
    pub capability: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    pub execution_mode: ExecutionMode,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<PluginArtifactRef>,
    #[serde(default)]
    pub compatibility_evidence: Vec<String>,
}

/// Immutable lock for a resolved logical PluginSet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetLock {
    pub contract_version: String,
    pub entries: Vec<ResolvedCapability>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<String>,
}

impl PluginManifest {
    pub fn artifact_reference(&self) -> Option<PluginArtifactRef> {
        self.artifact.clone()
    }
}

impl PluginSetLock {
    pub fn new(mut entries: Vec<ResolvedCapability>) -> Self {
        entries.sort_by(|left, right| {
            (
                &left.capability.id,
                &left.interface_version.version,
                &left.plugin.id,
                &left.plugin_version.version,
                left.execution_mode,
            )
                .cmp(&(
                    &right.capability.id,
                    &right.interface_version.version,
                    &right.plugin.id,
                    &right.plugin_version.version,
                    right.execution_mode,
                ))
        });
        Self {
            contract_version: "cyrene.plugin.v1".to_string(),
            entries,
            digest: None,
        }
    }

    pub fn with_digest(mut self) -> Result<Self, String> {
        self.digest = None;
        let value = serde_json::to_value(&self).map_err(|error| error.to_string())?;
        self.digest = Some(format!("sha256:{}", crate::canonical_sha256_hex(&value)));
        Ok(self)
    }
}
