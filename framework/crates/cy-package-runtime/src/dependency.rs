use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    ArtifactDigest, DependencyPreparationEvidence, PackageRuntimeError,
    descriptor::{digest_bytes, unix_ms},
};

/// Prepares one immutable runtime from the package's exact dependency lock.
///
/// Implementations write only beneath `runtime_root`. The package runtime
/// publishes that directory atomically and persists the returned evidence.
pub trait DependencyPreparer: Send + Sync {
    fn prepare(
        &self,
        package_root: &Path,
        runtime_root: &Path,
        lock_digest: &ArtifactDigest,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError>;
}

/// Production Python dependency preparation using a dedicated virtualenv and
/// exact, no-dependency-resolution lock consumption.
#[derive(Debug, Clone)]
pub struct PythonVenvDependencyPreparer {
    python_executable: PathBuf,
    uv_executable: PathBuf,
    offline: bool,
    wheelhouse: Option<PathBuf>,
}

impl PythonVenvDependencyPreparer {
    pub fn new(python_executable: impl Into<PathBuf>) -> Self {
        Self {
            python_executable: python_executable.into(),
            uv_executable: PathBuf::from("uv"),
            offline: false,
            wheelhouse: None,
        }
    }

    pub fn with_uv_executable(mut self, uv_executable: impl Into<PathBuf>) -> Self {
        self.uv_executable = uv_executable.into();
        self
    }

    pub fn offline(mut self, wheelhouse: impl Into<PathBuf>) -> Self {
        self.offline = true;
        self.wheelhouse = Some(wheelhouse.into());
        self
    }
}

impl DependencyPreparer for PythonVenvDependencyPreparer {
    fn prepare(
        &self,
        package_root: &Path,
        runtime_root: &Path,
        lock_digest: &ArtifactDigest,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError> {
        let lock = package_root.join("requirements.lock");
        let lock_bytes = fs::read(&lock).map_err(|error| {
            PackageRuntimeError::new(
                "DEPENDENCY_LOCK_MISSING",
                format!("could not read {}: {error}", lock.display()),
            )
        })?;
        if digest_bytes(&lock_bytes) != *lock_digest {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_LOCK_CORRUPT",
                "dependency lock changed before preparation",
            ));
        }
        validate_exact_requirements(&lock_bytes)?;
        fs::create_dir_all(runtime_root).map_err(|error| {
            PackageRuntimeError::new(
                "DEPENDENCY_PREPARE_FAILED",
                format!("could not create runtime root: {error}"),
            )
        })?;
        let output = Command::new(&self.uv_executable)
            .env("UV_CACHE_DIR", runtime_root.join(".uv-cache"))
            .arg("venv")
            .arg("--allow-existing")
            .arg("--python")
            .arg(&self.python_executable)
            .arg(runtime_root)
            .output()
            .map_err(|error| {
                PackageRuntimeError::new(
                    "DEPENDENCY_PREPARE_FAILED",
                    format!("could not start uv virtualenv preparation: {error}"),
                )
            })?;
        if !output.status.success() {
            return Err(command_failure("uv venv", &output));
        }

        let relative_python = if cfg!(windows) {
            PathBuf::from("Scripts/python.exe")
        } else {
            PathBuf::from("bin/python")
        };
        let runtime_python = runtime_root.join(&relative_python);
        let mut command = Command::new(&self.uv_executable);
        command.env("UV_CACHE_DIR", runtime_root.join(".uv-cache"));
        command.args(["pip", "install", "--python"]);
        command.arg(&runtime_python);
        command.args(["--no-deps", "--requirement"]);
        command.arg(&lock);
        if self.offline {
            command.arg("--no-index");
            let wheelhouse = self.wheelhouse.as_ref().ok_or_else(|| {
                PackageRuntimeError::new(
                    "DEPENDENCY_PREPARE_FAILED",
                    "offline dependency preparation requires a wheelhouse",
                )
            })?;
            command.arg("--find-links").arg(wheelhouse);
        }
        let output = command.output().map_err(|error| {
            PackageRuntimeError::new(
                "DEPENDENCY_PREPARE_FAILED",
                format!("could not start locked dependency installation: {error}"),
            )
        })?;
        if !output.status.success() {
            return Err(command_failure("uv pip install", &output));
        }

        let runtime_digest =
            digest_bytes(format!("python-venv-v1\n{}\n", lock_digest.as_str()).as_bytes());
        Ok(DependencyPreparationEvidence {
            preparer: "python-uv-venv-v1".to_string(),
            prepared_at_unix_ms: unix_ms(),
            lock_digest: lock_digest.clone(),
            runtime_digest,
            python_executable: Some(relative_python),
            python_paths: Vec::new(),
        })
    }
}

fn validate_exact_requirements(content: &[u8]) -> Result<(), PackageRuntimeError> {
    let text = std::str::from_utf8(content).map_err(|error| {
        PackageRuntimeError::new(
            "DEPENDENCY_LOCK_INVALID",
            format!("dependency lock is not UTF-8: {error}"),
        )
    })?;
    let requirements = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    if requirements.is_empty() {
        return Err(PackageRuntimeError::new(
            "DEPENDENCY_LOCK_INVALID",
            "dependency lock contains no exact pins",
        ));
    }
    if requirements.iter().any(|line| {
        !line.contains("==")
            || line.contains(['<', '>', '*', '@'])
            || line.contains("!=")
            || line.to_ascii_lowercase().contains("latest")
    }) {
        return Err(PackageRuntimeError::new(
            "DEPENDENCY_LOCK_INVALID",
            "dependency lock must contain exact, immutable pins only",
        ));
    }
    Ok(())
}

fn command_failure(operation: &str, output: &std::process::Output) -> PackageRuntimeError {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = format!("{} {}", stdout.trim(), stderr.trim());
    PackageRuntimeError::new(
        "DEPENDENCY_PREPARE_FAILED",
        format!("{operation} failed: {}", detail.trim()),
    )
}
