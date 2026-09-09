use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Deserialize;

use crate::{
    ArtifactDigest, DependencyPreparationEvidence, PackageRuntimeError, descriptor::unix_ms,
};

const PREPARER_PROTOCOL: &str = "cyrene.package-dependency-preparer.v1";
const MAX_PREPARER_OUTPUT_BYTES: usize = 64 * 1024;

/// Prepares one immutable runtime from a package's exact dependency lock.
///
/// Implementations are injected at the Platform boundary. Language-specific
/// preparation belongs to the Plugin repository or deployment integration.
pub trait DependencyPreparer: Send + Sync {
    fn prepare(
        &self,
        package_root: &Path,
        runtime_root: &Path,
        lock_digest: &ArtifactDigest,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError>;
}

/// Invokes a configured out-of-process dependency adapter.
///
/// Platform owns the process boundary and evidence validation. The adapter
/// owns language tooling and writes only below the supplied staging directory.
#[derive(Debug, Clone)]
pub struct CommandDependencyPreparer {
    executable: PathBuf,
    args: Vec<OsString>,
}

impl CommandDependencyPreparer {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = OsString>) -> Self {
        self.args = args.into_iter().collect();
        self
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationResult {
    protocol: String,
    preparer: String,
    runtime_digest: String,
    #[serde(default)]
    runtime_executable: Option<PathBuf>,
}

impl DependencyPreparer for CommandDependencyPreparer {
    fn prepare(
        &self,
        package_root: &Path,
        runtime_root: &Path,
        lock_digest: &ArtifactDigest,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError> {
        if self.executable.as_os_str().is_empty() {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_PREPARER_UNAVAILABLE",
                "dependency preparer executable is empty",
            ));
        }
        fs::create_dir_all(runtime_root).map_err(|error| {
            PackageRuntimeError::new(
                "DEPENDENCY_PREPARE_FAILED",
                format!("could not create runtime root: {error}"),
            )
        })?;
        let output = Command::new(&self.executable)
            .args(&self.args)
            .arg("--package-root")
            .arg(package_root)
            .arg("--runtime-root")
            .arg(runtime_root)
            .arg("--lock-digest")
            .arg(lock_digest.as_str())
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                PackageRuntimeError::new(
                    "DEPENDENCY_PREPARER_UNAVAILABLE",
                    format!("could not start configured dependency preparer: {error}"),
                )
            })?;
        if !output.status.success() {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_PREPARE_FAILED",
                format!(
                    "configured dependency preparer exited with status {}",
                    output.status
                ),
            ));
        }
        if output.stdout.len() > MAX_PREPARER_OUTPUT_BYTES {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_EVIDENCE_INVALID",
                "dependency preparer output exceeds 64 KiB",
            ));
        }
        let result: PreparationResult =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                PackageRuntimeError::new(
                    "DEPENDENCY_EVIDENCE_INVALID",
                    format!("dependency preparer returned invalid JSON evidence: {error}"),
                )
            })?;
        if result.protocol != PREPARER_PROTOCOL {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_EVIDENCE_INVALID",
                format!("dependency preparer protocol must be {PREPARER_PROTOCOL}"),
            ));
        }
        if result.preparer.trim().is_empty()
            || result.preparer.len() > 128
            || result.preparer.chars().any(char::is_control)
        {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_EVIDENCE_INVALID",
                "dependency preparer identity is invalid",
            ));
        }
        Ok(DependencyPreparationEvidence {
            preparer: result.preparer,
            prepared_at_unix_ms: unix_ms(),
            lock_digest: lock_digest.clone(),
            runtime_digest: ArtifactDigest::new(result.runtime_digest)?,
            runtime_executable: result.runtime_executable,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn command_adapter_accepts_only_versioned_generic_evidence() {
        let temporary = tempfile::tempdir().unwrap();
        let package_root = temporary.path().join("package");
        let runtime_root = temporary.path().join("runtime");
        fs::create_dir(&package_root).unwrap();
        let script = temporary.path().join("prepare.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nmkdir -p \"$4/bin\"\nprintf '#!/bin/sh\\n' > \"$4/bin/runtime\"\nprintf '%s\\n' '{{\"protocol\":\"{PREPARER_PROTOCOL}\",\"preparer\":\"test-adapter\",\"runtime_digest\":\"{DIGEST}\",\"runtime_executable\":\"bin/runtime\"}}'\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let evidence = CommandDependencyPreparer::new(script)
            .prepare(
                &package_root,
                &runtime_root,
                &ArtifactDigest::new(DIGEST).unwrap(),
            )
            .unwrap();

        assert_eq!(evidence.preparer, "test-adapter");
        assert_eq!(evidence.runtime_digest.as_str(), DIGEST);
        assert_eq!(
            evidence.runtime_executable,
            Some(PathBuf::from("bin/runtime"))
        );
    }
}
