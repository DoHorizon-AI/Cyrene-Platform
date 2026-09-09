use std::{collections::BTreeMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

macro_rules! identity_type {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, PackageRuntimeError> {
                let value = value.into();
                if !valid_identity(&value) {
                    return Err(PackageRuntimeError::new(
                        "IDENTITY_INVALID",
                        format!("{} is invalid: {value}", $label),
                    ));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identity_type!(PackageId, "package id");
identity_type!(InstallationId, "installation id");
identity_type!(CapabilityId, "capability id");
identity_type!(BindingId, "binding id");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageVersion(String);

impl PackageVersion {
    pub fn new(value: impl Into<String>) -> Result<Self, PackageRuntimeError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
        {
            return Err(PackageRuntimeError::new(
                "IDENTITY_INVALID",
                format!("package version is invalid: {value}"),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, PackageRuntimeError> {
        let value = value.into();
        if !valid_sha256(&value) {
            return Err(PackageRuntimeError::new(
                "DIGEST_INVALID",
                format!("invalid SHA-256 digest: {value}"),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn hex(&self) -> &str {
        self.0.strip_prefix("sha256:").expect("validated digest")
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub fn new(value: u64) -> Result<Self, PackageRuntimeError> {
        if value == 0 {
            return Err(PackageRuntimeError::new(
                "GENERATION_INVALID",
                "runtime generation must be positive",
            ));
        }
        Ok(Self(value))
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSource {
    pub descriptor_path: PathBuf,
    pub archive_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInspection {
    pub package_id: PackageId,
    pub package_version: PackageVersion,
    pub artifact_digest: ArtifactDigest,
    pub archive_digest: ArtifactDigest,
    pub dependency_lock_digest: ArtifactDigest,
    pub capabilities: Vec<CapabilityId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub verifier: String,
    pub verified_at_unix_ms: u128,
    pub artifact_digest: ArtifactDigest,
    pub archive_digest: ArtifactDigest,
    pub descriptor_digest: ArtifactDigest,
    pub manifest_digest: ArtifactDigest,
    pub dependency_lock_digest: ArtifactDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedPackage {
    pub inspection: PackageInspection,
    pub evidence: VerificationEvidence,
    pub source: PackageSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyPreparationEvidence {
    pub preparer: String,
    pub prepared_at_unix_ms: u128,
    pub lock_digest: ArtifactDigest,
    pub runtime_digest: ArtifactDigest,
    pub runtime_executable: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstallationState {
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationRecord {
    pub record_version: u32,
    pub installation_id: InstallationId,
    pub package_id: PackageId,
    pub package_version: PackageVersion,
    pub artifact_digest: ArtifactDigest,
    pub archive_digest: ArtifactDigest,
    pub capabilities: Vec<CapabilityId>,
    pub state: InstallationState,
    pub verification: VerificationEvidence,
    pub dependencies: DependencyPreparationEvidence,
    pub installed_at_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    pub binding_id: BindingId,
    pub installation_id: InstallationId,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeState {
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub binding_id: BindingId,
    pub installation_id: InstallationId,
    pub generation: RuntimeGeneration,
    pub state: RuntimeState,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub connection_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    pub staging_entries_removed: usize,
    pub cached_archives_removed: usize,
    pub dependency_runtimes_removed: usize,
    pub orphan_runtimes: usize,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct PackageRuntimeError {
    pub code: String,
    pub message: String,
}

impl PackageRuntimeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_identity(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}
