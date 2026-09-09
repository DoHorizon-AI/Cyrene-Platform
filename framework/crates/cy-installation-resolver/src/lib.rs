// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: framework/crates/cy-installation-resolver/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Framework-side adapter for verified local plugin installation records.
//!
//! Installation layout, OCI retrieval, signature checking, and supply-chain
//! verification are outside Kernel mechanisms. This adapter reads the compact
//! evidence an external installer wrote and returns a digest-bound launch plan
//! through the Kernel's resolver port.

use std::{collections::BTreeMap, fs, path::PathBuf};

use cy_kernel_api::{
    CgroupLimits, InstalledPluginResolver, LaunchPlan, ProviderError, ResolvedLaunchPlan,
    VerifiedInstallation,
};
use serde::Deserialize;

/// Reads installation records previously verified and written by an external
/// installer. This adapter never downloads artifacts or validates signatures.
#[derive(Debug, Clone)]
pub struct FilesystemInstalledPluginResolver {
    root: PathBuf,
    transport_root: PathBuf,
}

impl FilesystemInstalledPluginResolver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            transport_root: PathBuf::from("/run/cyrene/workers"),
        }
    }

    pub fn with_worker_transport_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.transport_root = root.into();
        self
    }
}

#[derive(Debug, Deserialize)]
struct InstalledLaunchRecord {
    record_version: u32,
    installation_name: String,
    manifest_digest: String,
    artifact_digest: String,
    verified_signature_identity: String,
    signature_policy_name: String,
    sbom_digest: String,
    provenance_digest: String,
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
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
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
        validate_installation_record(&record, installation)?;

        let manifest_path = installation_path.join("manifest.json");
        if manifest_path.exists() {
            let manifest_content = fs::read_to_string(&manifest_path).map_err(|error| {
                ProviderError::new(
                    "filesystem-plugin-resolver",
                    "INSTALLATION_MANIFEST_READ_FAILED",
                    &error.to_string(),
                )
            })?;
            let manifest: serde_json::Value =
                serde_json::from_str(&manifest_content).map_err(|error| {
                    ProviderError::new(
                        "filesystem-plugin-resolver",
                        "INSTALLATION_MANIFEST_INVALID",
                        &error.to_string(),
                    )
                })?;
            let manifest_id = manifest
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ProviderError::new(
                        "filesystem-plugin-resolver",
                        "INSTALLATION_MANIFEST_INVALID",
                        "repository Plugin manifest id is required",
                    )
                })?;
            if manifest_id != record.installation_name {
                return Err(ProviderError::new(
                    "filesystem-plugin-resolver",
                    "INSTALLATION_MANIFEST_ID_MISMATCH",
                    &format!(
                        "manifest id {} does not match installation name {}",
                        manifest_id, record.installation_name
                    ),
                ));
            }
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
        if !executable.is_file() {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "EXECUTABLE_NOT_FILE",
                &executable.display().to_string(),
            ));
        }

        Ok(ResolvedLaunchPlan {
            installation: installation.clone(),
            plan: LaunchPlan {
                instance_name: instance_name.to_string(),
                executable,
                args: record.args,
                environment: record.environment,
                cgroup_name: format!("instance-{instance_name}"),
                limits: CgroupLimits::default(),
                working_dir: Some(installation_path),
                transport_socket: Some(self.transport_root.join(format!("{instance_name}.sock"))),
            },
        })
    }

    fn resolve_worker_launch_plan(
        &self,
        worker: &cy_kernel_api::semantic::Worker,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        // The execution reference format is owned by this outer installation
        // adapter. Kernel code treats it as opaque and only sees the verified
        // ResolvedLaunchPlan returned below.
        let (installation_name, manifest_digest) =
            worker.execution_ref.split_once('@').ok_or_else(|| {
                ProviderError::new(
                    "filesystem-plugin-resolver",
                    "EXECUTION_REFERENCE_INVALID",
                    "expected installation-name@sha256:digest",
                )
            })?;
        if !safe_segment(installation_name) || !sha256_digest(manifest_digest) {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "EXECUTION_REFERENCE_INVALID",
                "execution reference has an unsafe installation name or digest",
            ));
        }
        let root = self.root.canonicalize().map_err(|error| {
            ProviderError::new(
                "filesystem-plugin-resolver",
                "INSTALLATION_ROOT_UNAVAILABLE",
                &error.to_string(),
            )
        })?;
        let installation_path = root
            .join(installation_name)
            .canonicalize()
            .map_err(|error| {
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
        let installation = VerifiedInstallation {
            installation_name: record.installation_name.clone(),
            manifest_digest: record.manifest_digest.clone(),
            artifact_digest: record.artifact_digest.clone(),
            verified_signature_identity: record.verified_signature_identity.clone(),
        };
        if installation.installation_name != installation_name
            || installation.manifest_digest != manifest_digest
        {
            return Err(ProviderError::new(
                "filesystem-plugin-resolver",
                "EXECUTION_REFERENCE_MISMATCH",
                "execution reference does not bind the verified installation record",
            ));
        }
        self.resolve_launch_plan(&installation, &worker.identity.id)
    }
}

fn validate_installation_record(
    record: &InstalledLaunchRecord,
    installation: &VerifiedInstallation,
) -> Result<(), ProviderError> {
    if record.record_version != 1 || !safe_segment(&record.installation_name) {
        return Err(ProviderError::new(
            "filesystem-plugin-resolver",
            "INSTALLATION_RECORD_VERSION_INVALID",
            "record_version=1 and a safe installation_name are required",
        ));
    }
    if record.installation_name != installation.installation_name
        || record.manifest_digest != installation.manifest_digest
        || record.artifact_digest != installation.artifact_digest
        || record.verified_signature_identity != installation.verified_signature_identity
    {
        return Err(ProviderError::new(
            "filesystem-plugin-resolver",
            "INSTALLATION_BINDING_MISMATCH",
            "installed record does not match the verified plugin reference",
        ));
    }
    if !sha256_digest(&record.manifest_digest)
        || !sha256_digest(&record.artifact_digest)
        || !sha256_digest(&record.sbom_digest)
        || !sha256_digest(&record.provenance_digest)
        || record.verified_signature_identity.trim().is_empty()
        || record.signature_policy_name.trim().is_empty()
    {
        return Err(ProviderError::new(
            "filesystem-plugin-resolver",
            "INSTALLATION_VERIFICATION_INVALID",
            "record must contain immutable digest, signature, SBOM, provenance, and policy evidence",
        ));
    }
    Ok(())
}

fn sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn installation() -> VerifiedInstallation {
        VerifiedInstallation {
            installation_name: "demo-plugin".to_string(),
            manifest_digest: DIGEST.to_string(),
            artifact_digest: DIGEST.to_string(),
            verified_signature_identity: "https://issuer.example/workload/demo".to_string(),
        }
    }

    fn write_record(root: &std::path::Path, signature_identity: &str, sbom_digest: &str) {
        let installation = root.join("demo-plugin");
        std::fs::create_dir_all(&installation).unwrap();
        std::fs::write(installation.join("worker"), b"worker").unwrap();
        std::fs::write(
            installation.join("launch.json"),
            serde_json::json!({
                "record_version": 1,
                "installation_name": "demo-plugin",
                "manifest_digest": DIGEST,
                "artifact_digest": DIGEST,
                "verified_signature_identity": signature_identity,
                "signature_policy_name": "production",
                "sbom_digest": sbom_digest,
                "provenance_digest": DIGEST,
                "executable": "worker",
                "args": ["--safe"],
                "environment": {"PLUGIN_MODE": "safe"}
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn resolves_only_a_fully_bound_verified_record() {
        let directory = tempfile::tempdir().unwrap();
        write_record(
            directory.path(),
            "https://issuer.example/workload/demo",
            DIGEST,
        );
        let resolved = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_launch_plan(&installation(), "instance-1")
            .unwrap();

        assert_eq!(resolved.installation, installation());
        assert_eq!(resolved.plan.args, vec!["--safe"]);
        assert!(resolved
            .plan
            .executable
            .starts_with(directory.path().canonicalize().unwrap()));
    }

    #[test]
    fn rejects_signature_or_supply_chain_evidence_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        write_record(directory.path(), "https://issuer.example/other", DIGEST);
        let error = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_launch_plan(&installation(), "instance-1")
            .unwrap_err();
        assert_eq!(error.reason_code, "INSTALLATION_BINDING_MISMATCH");

        write_record(
            directory.path(),
            "https://issuer.example/workload/demo",
            "sha256:not-a-valid-digest",
        );
        let error = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_launch_plan(&installation(), "instance-1")
            .unwrap_err();
        assert_eq!(error.reason_code, "INSTALLATION_VERIFICATION_INVALID");
    }

    #[test]
    fn resolves_worker_only_from_a_digest_bound_execution_reference() {
        let directory = tempfile::tempdir().unwrap();
        write_record(
            directory.path(),
            "https://issuer.example/workload/demo",
            DIGEST,
        );
        let worker = cy_kernel_api::semantic::Worker {
            identity: cy_kernel_api::semantic::Identity {
                id: "worker-1".to_string(),
                generation: 1,
            },
            principal: cy_kernel_api::semantic::Identity {
                id: "principal-1".to_string(),
                generation: 1,
            },
            provider: cy_kernel_api::semantic::Identity {
                id: "provider-1".to_string(),
                generation: 1,
            },
            lease: cy_kernel_api::semantic::Identity {
                id: "lease-1".to_string(),
                generation: 1,
            },
            state: cy_kernel_api::semantic::WorkerState::Starting,
            execution_ref: format!("demo-plugin@{DIGEST}"),
            limits: BTreeMap::new(),
        };
        let resolved = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_worker_launch_plan(&worker)
            .unwrap();
        assert_eq!(resolved.installation.manifest_digest, DIGEST);
        assert_eq!(resolved.plan.instance_name, "worker-1");
    }

    #[test]
    fn validates_optional_manifest_json_when_present() {
        let directory = tempfile::tempdir().unwrap();
        write_record(
            directory.path(),
            "https://issuer.example/workload/demo",
            DIGEST,
        );

        let manifest_content = r#"{
            "schemaVersion": 1,
            "id": "demo-plugin",
            "name": "Demo Plugin",
            "version": "1.0.0",
            "kind": "capability-plugin",
            "capabilities": ["example.v1"],
            "runtime": {"language": "python"}
        }"#;
        std::fs::write(
            directory.path().join("demo-plugin").join("manifest.json"),
            manifest_content,
        )
        .unwrap();

        // Valid manifest should pass
        let resolved = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_launch_plan(&installation(), "instance-1")
            .unwrap();
        assert_eq!(resolved.installation.installation_name, "demo-plugin");

        // Mismatched manifest ID should fail
        let bad_manifest =
            manifest_content.replace(r#""id": "demo-plugin""#, r#""id": "other-id""#);
        std::fs::write(
            directory.path().join("demo-plugin").join("manifest.json"),
            bad_manifest,
        )
        .unwrap();

        let error = FilesystemInstalledPluginResolver::new(directory.path())
            .resolve_launch_plan(&installation(), "instance-1")
            .unwrap_err();
        assert_eq!(error.reason_code, "INSTALLATION_MANIFEST_ID_MISMATCH");
    }
}
