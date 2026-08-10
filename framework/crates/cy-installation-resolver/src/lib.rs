//! Framework-side adapter for verified local plugin installation records.
//!
//! Installation layout, JSON parsing, and artifact record validation are not
//! Kernel mechanisms. The Kernel owns only the resolver port and validates the
//! resulting launch through its cgroup, lease, device-binding, and watchdog
//! path.

use std::{collections::BTreeMap, fs, path::PathBuf};

use cy_kernel_api::{
    CgroupLimits, InstalledPluginResolver, LaunchPlan, ProviderError, VerifiedInstallation,
};
use serde::Deserialize;

/// Reads installation records previously verified and written by the installer.
/// This adapter never downloads artifacts or validates signatures itself.
#[derive(Debug, Clone)]
pub struct FilesystemInstalledPluginResolver {
    root: PathBuf,
}

impl FilesystemInstalledPluginResolver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[derive(Debug, Deserialize)]
struct InstalledLaunchRecord {
    manifest_digest: String,
    artifact_digest: String,
    executable: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

impl InstalledPluginResolver for FilesystemInstalledPluginResolver {
    fn resolve_launch_plan(
        &self,
        installation: &VerifiedInstallation,
        instance_name: &str,
    ) -> Result<LaunchPlan, ProviderError> {
        if !safe_segment(&installation.installation_name) || !safe_segment(instance_name) {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_NAME_INVALID",
                &installation.installation_name,
            ));
        }
        let root = self.root.canonicalize().map_err(|error| {
            ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_ROOT_UNAVAILABLE",
                &error.to_string(),
            )
        })?;
        let installation_path = root.join(&installation.installation_name);
        let installation_path = installation_path.canonicalize().map_err(|error| {
            ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_NOT_FOUND",
                &error.to_string(),
            )
        })?;
        if !installation_path.starts_with(&root) {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_OUTSIDE_ROOT",
                &installation_path.display().to_string(),
            ));
        }
        let record =
            fs::read_to_string(installation_path.join("launch.json")).map_err(|error| {
                ProviderError::new(
                    "filesystem-plugin-resolver",
                    "INSTALLATION_RECORD_MISSING",
                    &error.to_string(),
                )
            })?;
        let record: InstalledLaunchRecord = serde_json::from_str(&record).map_err(|error| {
            ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_RECORD_INVALID",
                &error.to_string(),
            )
        })?;
        if record.manifest_digest != installation.manifest_digest
            || record.artifact_digest != installation.artifact_digest
        {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_DIGEST_MISMATCH",
                "installed record does not match the verified plugin reference",
            ));
        }
        let executable = if record.executable.is_absolute() {
            record.executable
        } else {
            installation_path.join(record.executable)
        };
        let executable = executable.canonicalize().map_err(|error| {
            ProviderError::new(
                "filesystem-plugin-resolver",
                "EXECUTABLE_NOT_FOUND",
                &error.to_string(),
            )
        })?;
        if !executable.starts_with(&installation_path) {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "EXECUTABLE_OUTSIDE_INSTALLATION",
                &executable.display().to_string(),
            ));
        }
        Ok(LaunchPlan {
            instance_name: instance_name.to_string(),
            executable,
            args: record.args,
            environment: record.environment,
            cgroup_name: format!("instance-{instance_name}"),
            limits: CgroupLimits::default(),
        })
    }
}

fn safe_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
