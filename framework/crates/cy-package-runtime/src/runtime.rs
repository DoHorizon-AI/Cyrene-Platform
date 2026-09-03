use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use cy_manifest::PluginManifest;
use cy_platform_api::WorkerActivationOptions;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    ActivationRequest, ArtifactDigest, BindingId, CleanupReport, DependencyPreparationEvidence,
    InstallationId, InstallationRecord, InstallationState, PackageId, PackageInspection,
    PackageRuntimeError, PackageSource, PackageVersion, RuntimeGeneration, RuntimeState,
    RuntimeStatus, VerifiedPackage, WorkerSupervisor,
    dependency::DependencyPreparer,
    descriptor::{
        digest_entries, digest_file, inspect_source, unix_ms, validate_archive_paths, verify_source,
    },
};

const INSTALLATION_RECORD_VERSION: u32 = 1;
const ACTIVATION_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActivationRecord {
    record_version: u32,
    binding_id: BindingId,
    installation_id: InstallationId,
    previous_installation_id: Option<InstallationId>,
    generation: RuntimeGeneration,
    desired_active: bool,
    last_failure_code: Option<String>,
    last_failure_message: Option<String>,
}

/// Durable filesystem package runtime with content-addressed cache and worker
/// activation delegated to the canonical Platform worker supervisor.
pub struct FilesystemPackageRuntime {
    root: PathBuf,
    dependency_preparer: Arc<dyn DependencyPreparer>,
    worker_supervisor: Mutex<Box<dyn WorkerSupervisor>>,
    base_worker_options: WorkerActivationOptions,
    lifecycle_lock: Mutex<()>,
}

impl FilesystemPackageRuntime {
    pub fn open(
        root: impl Into<PathBuf>,
        dependency_preparer: Arc<dyn DependencyPreparer>,
        worker_supervisor: Box<dyn WorkerSupervisor>,
        base_worker_options: WorkerActivationOptions,
    ) -> Result<Self, PackageRuntimeError> {
        let root = root.into();
        for directory in [
            root.join("cache/archives"),
            root.join("cache/descriptors"),
            root.join("dependencies"),
            root.join("installations"),
            root.join("bindings"),
            root.join("staging"),
        ] {
            fs::create_dir_all(&directory).map_err(|error| {
                PackageRuntimeError::new(
                    "RUNTIME_ROOT_UNAVAILABLE",
                    format!("could not create {}: {error}", directory.display()),
                )
            })?;
        }
        let runtime = Self {
            root,
            dependency_preparer,
            worker_supervisor: Mutex::new(worker_supervisor),
            base_worker_options,
            lifecycle_lock: Mutex::new(()),
        };
        runtime.recover_incomplete_transactions()?;
        Ok(runtime)
    }

    pub fn inspect(
        &self,
        source: &PackageSource,
    ) -> Result<PackageInspection, PackageRuntimeError> {
        Ok(inspect_source(source)?.inspection)
    }

    pub fn verify(&self, source: &PackageSource) -> Result<VerifiedPackage, PackageRuntimeError> {
        verify_source(source)
    }

    pub fn install(
        &self,
        source: &PackageSource,
    ) -> Result<InstallationRecord, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        self.install_locked(source)
    }

    pub fn install_offline(
        &self,
        package_id: &PackageId,
        package_version: &PackageVersion,
    ) -> Result<InstallationRecord, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        let mut candidates = Vec::new();
        for entry in read_directory(&self.root.join("cache/descriptors"))? {
            let descriptor_path = entry.path();
            if !descriptor_path.is_file() {
                continue;
            }
            let descriptor_bytes =
                fs::read(&descriptor_path).map_err(io_error("CACHE_READ_FAILED"))?;
            let value: serde_json::Value =
                serde_json::from_slice(&descriptor_bytes).map_err(|error| {
                    PackageRuntimeError::new(
                        "CACHE_CORRUPT",
                        format!("invalid cached descriptor: {error}"),
                    )
                })?;
            if value
                .pointer("/package/id")
                .and_then(serde_json::Value::as_str)
                != Some(package_id.as_str())
                || value
                    .pointer("/package/version")
                    .and_then(serde_json::Value::as_str)
                    != Some(package_version.as_str())
            {
                continue;
            }
            let archive_digest = value
                .pointer("/integrity/archive_digest")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    PackageRuntimeError::new("CACHE_CORRUPT", "cached archive digest is missing")
                })?;
            let archive_digest = ArtifactDigest::new(archive_digest.to_string())?;
            candidates.push(PackageSource {
                descriptor_path,
                archive_path: self.cache_archive_path(&archive_digest),
            });
        }
        if candidates.len() != 1 {
            return Err(PackageRuntimeError::new(
                if candidates.is_empty() {
                    "OFFLINE_PACKAGE_UNAVAILABLE"
                } else {
                    "OFFLINE_PACKAGE_AMBIGUOUS"
                },
                format!(
                    "offline cache has {} candidates for {}@{}",
                    candidates.len(),
                    package_id,
                    package_version
                ),
            ));
        }
        self.install_locked(&candidates.remove(0))
    }

    pub fn get_installation(
        &self,
        installation_id: &InstallationId,
    ) -> Result<InstallationRecord, PackageRuntimeError> {
        read_json(
            &self.installation_record_path(installation_id),
            "INSTALLATION_NOT_FOUND",
        )
    }

    pub fn list_installations(&self) -> Result<Vec<InstallationRecord>, PackageRuntimeError> {
        let mut records = Vec::new();
        for entry in read_directory(&self.root.join("installations"))? {
            if !entry.path().is_dir() {
                continue;
            }
            records.push(read_json(
                &entry.path().join("record.json"),
                "INSTALLATION_RECORD_INVALID",
            )?);
        }
        records.sort_by(|left: &InstallationRecord, right| {
            left.installation_id.cmp(&right.installation_id)
        });
        Ok(records)
    }

    pub fn activate(
        &self,
        request: ActivationRequest,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        let previous = self.read_activation_optional(&request.binding_id)?;
        let generation = RuntimeGeneration::new(
            previous
                .as_ref()
                .map_or(1, |record| record.generation.value() + 1),
        )?;
        self.activate_worker(&request, generation)?;
        let record = ActivationRecord {
            record_version: ACTIVATION_RECORD_VERSION,
            binding_id: request.binding_id.clone(),
            installation_id: request.installation_id.clone(),
            previous_installation_id: previous.map(|record| record.installation_id),
            generation,
            desired_active: true,
            last_failure_code: None,
            last_failure_message: None,
        };
        if let Err(error) = self.write_activation(&record) {
            let _ = self.supervisor()?.deactivate(&request.binding_id);
            return Err(error);
        }
        Ok(RuntimeStatus {
            binding_id: request.binding_id,
            installation_id: request.installation_id,
            generation,
            state: RuntimeState::Running,
            failure_code: None,
            failure_message: None,
        })
    }

    /// Recover one Product-authorized binding after process restart. Secret
    /// values are supplied again and are never persisted by this runtime.
    pub fn recover_binding(
        &self,
        binding_id: &BindingId,
        environment: BTreeMap<String, String>,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        let record = self.read_activation(binding_id)?;
        if !record.desired_active {
            return Err(PackageRuntimeError::new(
                "BINDING_NOT_ENABLED_FOR_RECOVERY",
                format!("binding {binding_id} is not marked active"),
            ));
        }
        self.activate(ActivationRequest {
            binding_id: binding_id.clone(),
            installation_id: record.installation_id,
            environment,
        })
    }

    pub fn deactivate(&self, binding_id: &BindingId) -> Result<RuntimeStatus, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        let mut record = self.read_activation(binding_id)?;
        self.supervisor()?.deactivate(binding_id)?;
        record.desired_active = false;
        record.last_failure_code = None;
        record.last_failure_message = None;
        self.write_activation(&record)?;
        Ok(RuntimeStatus {
            binding_id: binding_id.clone(),
            installation_id: record.installation_id,
            generation: record.generation,
            state: RuntimeState::Stopped,
            failure_code: None,
            failure_message: None,
        })
    }

    pub fn runtime_status(
        &self,
        binding_id: &BindingId,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        let record = self.read_activation(binding_id)?;
        let state = self.supervisor()?.status(binding_id)?;
        let state = if record.desired_active && state == RuntimeState::Stopped {
            RuntimeState::Failed
        } else {
            state
        };
        Ok(RuntimeStatus {
            binding_id: binding_id.clone(),
            installation_id: record.installation_id,
            generation: record.generation,
            state,
            failure_code: record.last_failure_code,
            failure_message: record.last_failure_message,
        })
    }

    /// Invoke an already-supervised worker through the existing canonical
    /// worker protocol. Product adapters should normally expose this through
    /// CES rather than leaking this internal seam into Product DTOs.
    pub fn invoke(
        &self,
        binding_id: &BindingId,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, PackageRuntimeError> {
        self.supervisor()?
            .invoke(binding_id, capability, method, payload, timeout)
    }

    pub fn invoke_typed(
        &self,
        binding_id: &BindingId,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<crate::RuntimeInvocationResult, PackageRuntimeError> {
        self.supervisor()?
            .invoke_typed(binding_id, capability, method, payload, timeout)
    }

    pub fn subscribe(
        &self,
        binding_id: &BindingId,
        capability: &str,
        filter_payload: &[u8],
        timeout: Duration,
    ) -> Result<String, PackageRuntimeError> {
        self.supervisor()?
            .subscribe(binding_id, capability, filter_payload, timeout)
    }

    pub fn next_event(
        &self,
        binding_id: &BindingId,
        subscription_id: &str,
        timeout: Duration,
    ) -> Result<Option<crate::RuntimeApplicationEvent>, PackageRuntimeError> {
        self.supervisor()?
            .next_event(binding_id, subscription_id, timeout)
    }

    pub fn unsubscribe(
        &self,
        binding_id: &BindingId,
        subscription_id: &str,
        timeout: Duration,
    ) -> Result<(), PackageRuntimeError> {
        self.supervisor()?
            .unsubscribe(binding_id, subscription_id, timeout)
    }

    pub fn upgrade(
        &self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        environment: BTreeMap<String, String>,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        self.switch_installation(binding_id, installation_id, environment, false)
    }

    pub fn rollback(
        &self,
        binding_id: &BindingId,
        environment: BTreeMap<String, String>,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        let current = self.read_activation(binding_id)?;
        let previous = current.previous_installation_id.clone().ok_or_else(|| {
            PackageRuntimeError::new(
                "ROLLBACK_UNAVAILABLE",
                format!("no rollback target for {binding_id}"),
            )
        })?;
        self.switch_installation(binding_id, &previous, environment, true)
    }

    pub fn remove_binding_reference(
        &self,
        binding_id: &BindingId,
    ) -> Result<(), PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        if self.supervisor()?.status(binding_id)? == RuntimeState::Running {
            return Err(PackageRuntimeError::new(
                "BINDING_RUNNING",
                format!("deactivate {binding_id} before removing its runtime reference"),
            ));
        }
        let path = self.activation_path(binding_id);
        if !path.is_file() {
            return Err(PackageRuntimeError::new(
                "BINDING_NOT_FOUND",
                format!("unknown binding: {binding_id}"),
            ));
        }
        fs::remove_file(path).map_err(io_error("BINDING_REMOVE_FAILED"))
    }

    pub fn uninstall(&self, installation_id: &InstallationId) -> Result<(), PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        for record in self.list_activation_records()? {
            if record.installation_id == *installation_id
                || record.previous_installation_id.as_ref() == Some(installation_id)
            {
                return Err(PackageRuntimeError::new(
                    "INSTALLATION_REFERENCED",
                    format!(
                        "installation {installation_id} is referenced by {}",
                        record.binding_id
                    ),
                ));
            }
        }
        let path = self.installation_path(installation_id);
        if !path.is_dir() {
            return Err(PackageRuntimeError::new(
                "INSTALLATION_NOT_FOUND",
                format!("installation is not present: {installation_id}"),
            ));
        }
        fs::remove_dir_all(path).map_err(io_error("UNINSTALL_FAILED"))
    }

    pub fn cleanup(&self) -> Result<CleanupReport, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        let mut report = CleanupReport {
            staging_entries_removed: 0,
            cached_archives_removed: 0,
            dependency_runtimes_removed: 0,
            orphan_runtimes: 0,
        };
        for entry in read_directory(&self.root.join("staging"))? {
            remove_entry(&entry.path())?;
            report.staging_entries_removed += 1;
        }

        let activation_records = self.list_activation_records()?;
        let desired = activation_records
            .iter()
            .filter(|record| record.desired_active)
            .map(|record| record.binding_id.clone())
            .collect::<BTreeSet<_>>();
        let active = self.supervisor()?.active_bindings()?;
        for orphan in active.difference(&desired) {
            self.supervisor()?.deactivate(orphan)?;
        }
        report.orphan_runtimes = self
            .supervisor()?
            .active_bindings()?
            .difference(&desired)
            .count();

        let installations = self.list_installations()?;
        let archive_digests = installations
            .iter()
            .map(|record| record.archive_digest.clone())
            .collect::<BTreeSet<_>>();
        let dependency_digests = installations
            .iter()
            .map(|record| record.dependencies.lock_digest.clone())
            .collect::<BTreeSet<_>>();
        for entry in read_directory(&self.root.join("cache/archives"))? {
            let keep = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".zip"))
                .and_then(|hex| ArtifactDigest::new(format!("sha256:{hex}")).ok())
                .is_some_and(|digest| archive_digests.contains(&digest));
            if !keep {
                remove_entry(&entry.path())?;
                report.cached_archives_removed += 1;
            }
        }
        for entry in read_directory(&self.root.join("cache/descriptors"))? {
            let keep = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|name| InstallationId::new(name.to_string()).ok())
                .is_some_and(|installation_id| {
                    installations
                        .iter()
                        .any(|record| record.installation_id == installation_id)
                });
            if !keep {
                remove_entry(&entry.path())?;
            }
        }
        for entry in read_directory(&self.root.join("dependencies"))? {
            if entry.file_name().to_string_lossy().starts_with('.') {
                remove_entry(&entry.path())?;
                report.dependency_runtimes_removed += 1;
                continue;
            }
            let keep =
                ArtifactDigest::new(format!("sha256:{}", entry.file_name().to_string_lossy()))
                    .is_ok_and(|digest| dependency_digests.contains(&digest));
            if !keep {
                remove_entry(&entry.path())?;
                report.dependency_runtimes_removed += 1;
            }
        }
        Ok(report)
    }

    pub fn orphan_runtime_count(&self) -> Result<usize, PackageRuntimeError> {
        let desired = self
            .list_activation_records()?
            .into_iter()
            .filter(|record| record.desired_active)
            .map(|record| record.binding_id)
            .collect::<BTreeSet<_>>();
        Ok(self
            .supervisor()?
            .active_bindings()?
            .difference(&desired)
            .count())
    }

    fn install_locked(
        &self,
        source: &PackageSource,
    ) -> Result<InstallationRecord, PackageRuntimeError> {
        let verified = verify_source(source)?;
        let installation_id = installation_id(&verified);
        if let Ok(record) = self.get_installation(&installation_id) {
            if record.verification == verified.evidence {
                return Ok(record);
            }
            if record.artifact_digest == verified.inspection.artifact_digest
                && record.archive_digest == verified.inspection.archive_digest
            {
                return Ok(record);
            }
            return Err(PackageRuntimeError::new(
                "INSTALLATION_ID_CONFLICT",
                format!("installation identity collision: {installation_id}"),
            ));
        }
        self.cache_verified_source(&verified, &installation_id)?;
        let stage = self.root.join("staging").join(format!(
            "{}-{}",
            installation_id.as_str(),
            Uuid::new_v4()
        ));
        fs::create_dir(&stage).map_err(io_error("INSTALL_STAGE_FAILED"))?;
        let result = (|| {
            let payload = stage.join("payload");
            fs::create_dir(&payload).map_err(io_error("INSTALL_STAGE_FAILED"))?;
            self.extract_archive(&verified, &payload)?;
            let manifest = read_platform_manifest(&payload)?;
            validate_installed_manifest(&verified, &manifest)?;
            let dependencies = self.prepare_dependencies(&verified, &payload)?;
            let record = InstallationRecord {
                record_version: INSTALLATION_RECORD_VERSION,
                installation_id: installation_id.clone(),
                package_id: verified.inspection.package_id.clone(),
                package_version: verified.inspection.package_version.clone(),
                artifact_digest: verified.inspection.artifact_digest.clone(),
                archive_digest: verified.inspection.archive_digest.clone(),
                capabilities: verified.inspection.capabilities.clone(),
                state: InstallationState::Installed,
                verification: verified.evidence.clone(),
                dependencies,
                installed_at_unix_ms: unix_ms(),
            };
            write_json(&stage.join("record.json"), &record)?;
            sync_directory(&stage)?;
            let destination = self.installation_path(&installation_id);
            match fs::rename(&stage, &destination) {
                Ok(()) => {
                    sync_directory(&self.root.join("installations"))?;
                    Ok(record)
                }
                Err(_error) if destination.is_dir() => self.get_installation(&installation_id),
                Err(error) => Err(PackageRuntimeError::new(
                    "INSTALL_PUBLISH_FAILED",
                    format!("atomic installation publication failed: {error}"),
                )),
            }
        })();
        if result.is_err() && stage.exists() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }

    fn cache_verified_source(
        &self,
        package: &VerifiedPackage,
        installation_id: &InstallationId,
    ) -> Result<(), PackageRuntimeError> {
        let archive = self.cache_archive_path(&package.inspection.archive_digest);
        copy_immutable(
            &package.source.archive_path,
            &archive,
            &package.inspection.archive_digest,
        )?;
        let descriptor = self
            .root
            .join("cache/descriptors")
            .join(format!("{}.json", installation_id.as_str()));
        copy_immutable_bytes(
            &fs::read(&package.source.descriptor_path)
                .map_err(io_error("DESCRIPTOR_UNAVAILABLE"))?,
            &descriptor,
        )
    }

    fn extract_archive(
        &self,
        package: &VerifiedPackage,
        destination: &Path,
    ) -> Result<(), PackageRuntimeError> {
        let archive_path = self.cache_archive_path(&package.inspection.archive_digest);
        if digest_file(&archive_path)? != package.inspection.archive_digest {
            return Err(PackageRuntimeError::new(
                "CACHE_CORRUPT",
                "cached archive digest mismatch",
            ));
        }
        let file = File::open(&archive_path).map_err(io_error("CACHE_READ_FAILED"))?;
        let mut archive = ZipArchive::new(file).map_err(|error| {
            PackageRuntimeError::new("ARCHIVE_INVALID", format!("invalid ZIP package: {error}"))
        })?;
        validate_archive_paths(&mut archive)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| {
                PackageRuntimeError::new("ARCHIVE_INVALID", format!("invalid ZIP entry: {error}"))
            })?;
            let relative = entry.enclosed_name().ok_or_else(|| {
                PackageRuntimeError::new(
                    "ARCHIVE_TRAVERSAL_REJECTED",
                    "ZIP entry escapes destination",
                )
            })?;
            let target = destination.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(&target).map_err(io_error("ARCHIVE_EXTRACT_FAILED"))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io_error("ARCHIVE_EXTRACT_FAILED"))?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(io_error("ARCHIVE_EXTRACT_FAILED"))?;
            std::io::copy(&mut entry, &mut output).map_err(io_error("ARCHIVE_EXTRACT_FAILED"))?;
            output
                .sync_all()
                .map_err(io_error("ARCHIVE_EXTRACT_FAILED"))?;
        }
        let entries = read_tree_entries(destination)?;
        if digest_entries(&entries) != package.inspection.artifact_digest {
            return Err(PackageRuntimeError::new(
                "ARTIFACT_CORRUPT",
                "extracted artifact digest does not match verification evidence",
            ));
        }
        Ok(())
    }

    fn prepare_dependencies(
        &self,
        package: &VerifiedPackage,
        payload: &Path,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError> {
        let final_root = self.dependency_path(&package.inspection.dependency_lock_digest);
        let evidence_path = final_root.join("evidence.json");
        if evidence_path.is_file() {
            let evidence: DependencyPreparationEvidence =
                read_json(&evidence_path, "DEPENDENCY_EVIDENCE_INVALID")?;
            if evidence.lock_digest != package.inspection.dependency_lock_digest {
                return Err(PackageRuntimeError::new(
                    "DEPENDENCY_EVIDENCE_INVALID",
                    "prepared runtime evidence has the wrong lock digest",
                ));
            }
            return Ok(evidence);
        }
        let stage = self
            .root
            .join("dependencies")
            .join(format!(".stage-{}", Uuid::new_v4()));
        fs::create_dir(&stage).map_err(io_error("DEPENDENCY_PREPARE_FAILED"))?;
        let result = (|| {
            let evidence = self.dependency_preparer.prepare(
                payload,
                &stage,
                &package.inspection.dependency_lock_digest,
            )?;
            if evidence.lock_digest != package.inspection.dependency_lock_digest {
                return Err(PackageRuntimeError::new(
                    "DEPENDENCY_EVIDENCE_INVALID",
                    "dependency preparer returned evidence for a different lock",
                ));
            }
            validate_relative_evidence_paths(&evidence)?;
            write_json(&stage.join("evidence.json"), &evidence)?;
            sync_directory(&stage)?;
            match fs::rename(&stage, &final_root) {
                Ok(()) => Ok(evidence),
                Err(_) if evidence_path.is_file() => {
                    read_json(&evidence_path, "DEPENDENCY_EVIDENCE_INVALID")
                }
                Err(error) => Err(PackageRuntimeError::new(
                    "DEPENDENCY_PUBLISH_FAILED",
                    format!("atomic dependency publication failed: {error}"),
                )),
            }
        })();
        if result.is_err() && stage.exists() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }

    fn activate_worker(
        &self,
        request: &ActivationRequest,
        generation: RuntimeGeneration,
    ) -> Result<(), PackageRuntimeError> {
        let record = self.get_installation(&request.installation_id)?;
        let payload = self
            .installation_path(&request.installation_id)
            .join("payload");
        self.verify_installed_integrity(&record, &payload)?;
        let manifest = read_platform_manifest(&payload)?;
        validate_installed_manifest_record(&record, &manifest)?;
        let dependency_root = self.dependency_path(&record.dependencies.lock_digest);
        let mut options = self.base_worker_options.clone();
        options.working_dir = Some(payload.clone());
        options.python_path.push(payload.join("src"));
        for path in &record.dependencies.python_paths {
            options.python_path.push(dependency_root.join(path));
        }
        if let Some(executable) = &record.dependencies.python_executable {
            options.python_executable = Some(
                dependency_root
                    .join(executable)
                    .to_string_lossy()
                    .to_string(),
            );
        }
        options.environment.extend(request.environment.clone());
        options.environment.insert(
            "CYRENE_CAPABILITY_BINDING_ID".to_string(),
            request.binding_id.to_string(),
        );
        options
            .environment
            .insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
        self.supervisor()?.activate(
            &request.binding_id,
            &request.installation_id,
            &manifest,
            options,
            generation,
        )
    }

    fn switch_installation(
        &self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        environment: BTreeMap<String, String>,
        rollback: bool,
    ) -> Result<RuntimeStatus, PackageRuntimeError> {
        let _guard = self.lock_lifecycle()?;
        self.get_installation(installation_id)?;
        let current = self.read_activation(binding_id)?;
        let generation = RuntimeGeneration::new(current.generation.value() + 1)?;
        self.supervisor()?.deactivate(binding_id)?;
        let request = ActivationRequest {
            binding_id: binding_id.clone(),
            installation_id: installation_id.clone(),
            environment: environment.clone(),
        };
        if let Err(error) = self.activate_worker(&request, generation) {
            let restore = ActivationRequest {
                binding_id: binding_id.clone(),
                installation_id: current.installation_id.clone(),
                environment,
            };
            let _ = self.activate_worker(&restore, current.generation);
            return Err(error);
        }
        let record = ActivationRecord {
            record_version: ACTIVATION_RECORD_VERSION,
            binding_id: binding_id.clone(),
            installation_id: installation_id.clone(),
            previous_installation_id: Some(current.installation_id),
            generation,
            desired_active: true,
            last_failure_code: None,
            last_failure_message: None,
        };
        if let Err(error) = self.write_activation(&record) {
            let _ = self.supervisor()?.deactivate(binding_id);
            return Err(error);
        }
        let _ = rollback;
        Ok(RuntimeStatus {
            binding_id: binding_id.clone(),
            installation_id: installation_id.clone(),
            generation,
            state: RuntimeState::Running,
            failure_code: None,
            failure_message: None,
        })
    }

    fn recover_incomplete_transactions(&self) -> Result<(), PackageRuntimeError> {
        for entry in read_directory(&self.root.join("staging"))? {
            remove_entry(&entry.path())?;
        }
        for entry in read_directory(&self.root.join("dependencies"))? {
            if entry.file_name().to_string_lossy().starts_with(".stage-") {
                remove_entry(&entry.path())?;
            }
        }
        Ok(())
    }

    fn verify_installed_integrity(
        &self,
        record: &InstallationRecord,
        payload: &Path,
    ) -> Result<(), PackageRuntimeError> {
        if digest_entries(&read_tree_entries(payload)?) != record.artifact_digest {
            return Err(PackageRuntimeError::new(
                "INSTALLATION_CORRUPT",
                format!(
                    "installed payload does not match {}",
                    record.artifact_digest
                ),
            ));
        }
        let dependency_root = self.dependency_path(&record.dependencies.lock_digest);
        let evidence: DependencyPreparationEvidence = read_json(
            &dependency_root.join("evidence.json"),
            "DEPENDENCY_EVIDENCE_INVALID",
        )?;
        if evidence != record.dependencies {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_EVIDENCE_INVALID",
                "prepared dependency evidence differs from the installation record",
            ));
        }
        if let Some(executable) = &evidence.python_executable
            && !dependency_root.join(executable).is_file()
        {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_RUNTIME_CORRUPT",
                "prepared Python executable is missing",
            ));
        }
        if evidence
            .python_paths
            .iter()
            .any(|path| !dependency_root.join(path).exists())
        {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_RUNTIME_CORRUPT",
                "prepared Python path is missing",
            ));
        }
        Ok(())
    }

    fn list_activation_records(&self) -> Result<Vec<ActivationRecord>, PackageRuntimeError> {
        let mut records = Vec::new();
        for entry in read_directory(&self.root.join("bindings"))? {
            if entry.path().is_file() {
                records.push(read_json(&entry.path(), "BINDING_RECORD_INVALID")?);
            }
        }
        Ok(records)
    }

    fn read_activation(
        &self,
        binding_id: &BindingId,
    ) -> Result<ActivationRecord, PackageRuntimeError> {
        read_json(&self.activation_path(binding_id), "BINDING_NOT_FOUND")
    }

    fn read_activation_optional(
        &self,
        binding_id: &BindingId,
    ) -> Result<Option<ActivationRecord>, PackageRuntimeError> {
        let path = self.activation_path(binding_id);
        if !path.is_file() {
            return Ok(None);
        }
        read_json(&path, "BINDING_RECORD_INVALID").map(Some)
    }

    fn write_activation(&self, record: &ActivationRecord) -> Result<(), PackageRuntimeError> {
        write_json_atomic(&self.activation_path(&record.binding_id), record)
    }

    fn supervisor(&self) -> Result<MutexGuard<'_, Box<dyn WorkerSupervisor>>, PackageRuntimeError> {
        self.worker_supervisor.lock().map_err(|_| {
            PackageRuntimeError::new(
                "WORKER_SUPERVISOR_UNAVAILABLE",
                "worker supervisor lock is poisoned",
            )
        })
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, ()>, PackageRuntimeError> {
        self.lifecycle_lock.lock().map_err(|_| {
            PackageRuntimeError::new(
                "PACKAGE_RUNTIME_UNAVAILABLE",
                "package lifecycle lock is poisoned",
            )
        })
    }

    fn installation_path(&self, installation_id: &InstallationId) -> PathBuf {
        self.root
            .join("installations")
            .join(installation_id.as_str())
    }

    fn installation_record_path(&self, installation_id: &InstallationId) -> PathBuf {
        self.installation_path(installation_id).join("record.json")
    }

    fn activation_path(&self, binding_id: &BindingId) -> PathBuf {
        self.root
            .join("bindings")
            .join(format!("{}.json", binding_id.as_str()))
    }

    fn cache_archive_path(&self, digest: &ArtifactDigest) -> PathBuf {
        self.root
            .join("cache/archives")
            .join(format!("{}.zip", digest.hex()))
    }

    fn dependency_path(&self, digest: &ArtifactDigest) -> PathBuf {
        self.root.join("dependencies").join(digest.hex())
    }
}

fn installation_id(package: &VerifiedPackage) -> InstallationId {
    let identity = format!(
        "{}\0{}\0{}",
        package.inspection.package_id,
        package.inspection.package_version,
        package.inspection.artifact_digest
    );
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    InstallationId::new(format!("installation-{}", &digest[..32]))
        .expect("derived installation identity is valid")
}

fn read_platform_manifest(payload: &Path) -> Result<PluginManifest, PackageRuntimeError> {
    let bytes =
        fs::read(payload.join("plugin.manifest.json")).map_err(io_error("MANIFEST_MISSING"))?;
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        PackageRuntimeError::new(
            "MANIFEST_INVALID",
            format!("invalid official manifest: {error}"),
        )
    })?;
    cy_platform_api::normalize_official_manifest(value)
        .map_err(|error| PackageRuntimeError::new("MANIFEST_INVALID", error))
}

fn validate_installed_manifest(
    package: &VerifiedPackage,
    manifest: &PluginManifest,
) -> Result<(), PackageRuntimeError> {
    if manifest.plugin.id != package.inspection.package_id.as_str()
        || manifest.plugin.version != package.inspection.package_version.as_str()
    {
        return Err(PackageRuntimeError::new(
            "PACKAGE_IDENTITY_MISMATCH",
            "installed official manifest does not match verified package identity",
        ));
    }
    Ok(())
}

fn validate_installed_manifest_record(
    record: &InstallationRecord,
    manifest: &PluginManifest,
) -> Result<(), PackageRuntimeError> {
    if manifest.plugin.id != record.package_id.as_str()
        || manifest.plugin.version != record.package_version.as_str()
    {
        return Err(PackageRuntimeError::new(
            "INSTALLATION_CORRUPT",
            "installed manifest does not match durable installation record",
        ));
    }
    Ok(())
}

fn validate_relative_evidence_paths(
    evidence: &DependencyPreparationEvidence,
) -> Result<(), PackageRuntimeError> {
    let paths = evidence
        .python_executable
        .iter()
        .chain(evidence.python_paths.iter());
    if paths.clone().any(|path| {
        path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
    }) {
        return Err(PackageRuntimeError::new(
            "DEPENDENCY_EVIDENCE_INVALID",
            "dependency runtime paths must remain relative to the prepared runtime",
        ));
    }
    Ok(())
}

fn read_tree_entries(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, PackageRuntimeError> {
    fn visit(
        root: &Path,
        current: &Path,
        entries: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), PackageRuntimeError> {
        for entry in read_directory(current)? {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(io_error("ARTIFACT_READ_FAILED"))?;
            if metadata.file_type().is_symlink() {
                return Err(PackageRuntimeError::new(
                    "ARCHIVE_SYMLINK_REJECTED",
                    format!(
                        "installed payload contains a symbolic link: {}",
                        path.display()
                    ),
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, entries)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("walk remains below root")
                    .to_string_lossy()
                    .replace('\\', "/");
                entries.insert(
                    relative,
                    fs::read(&path).map_err(io_error("ARTIFACT_READ_FAILED"))?,
                );
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

fn copy_immutable(
    source: &Path,
    destination: &Path,
    digest: &ArtifactDigest,
) -> Result<(), PackageRuntimeError> {
    if destination.is_file() {
        if digest_file(destination)? == *digest {
            return Ok(());
        }
        return Err(PackageRuntimeError::new(
            "CACHE_CORRUPT",
            format!(
                "content-addressed cache entry differs: {}",
                destination.display()
            ),
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io_error("CACHE_WRITE_FAILED"))?;
    }
    let temporary = destination.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::copy(source, &temporary).map_err(io_error("CACHE_WRITE_FAILED"))?;
    if digest_file(&temporary)? != *digest {
        let _ = fs::remove_file(&temporary);
        return Err(PackageRuntimeError::new(
            "ARCHIVE_CORRUPT",
            "source changed while caching",
        ));
    }
    match fs::rename(&temporary, destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.is_file() && digest_file(destination)? == *digest => {
            let _ = fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(PackageRuntimeError::new(
                "CACHE_WRITE_FAILED",
                error.to_string(),
            ))
        }
    }
}

fn copy_immutable_bytes(content: &[u8], destination: &Path) -> Result<(), PackageRuntimeError> {
    if destination.is_file() {
        if fs::read(destination).map_err(io_error("CACHE_READ_FAILED"))? == content {
            return Ok(());
        }
        return Err(PackageRuntimeError::new(
            "CACHE_CONFLICT",
            format!("cached descriptor differs: {}", destination.display()),
        ));
    }
    write_bytes_atomic(destination, content)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), PackageRuntimeError> {
    let content = serde_json::to_vec_pretty(value)
        .map_err(|error| PackageRuntimeError::new("STATE_SERIALIZE_FAILED", error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error("STATE_WRITE_FAILED"))?;
    file.write_all(&content)
        .map_err(io_error("STATE_WRITE_FAILED"))?;
    file.write_all(b"\n")
        .map_err(io_error("STATE_WRITE_FAILED"))?;
    file.sync_all().map_err(io_error("STATE_WRITE_FAILED"))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), PackageRuntimeError> {
    let mut content = serde_json::to_vec_pretty(value)
        .map_err(|error| PackageRuntimeError::new("STATE_SERIALIZE_FAILED", error.to_string()))?;
    content.push(b'\n');
    write_bytes_atomic(path, &content)
}

fn write_bytes_atomic(path: &Path, content: &[u8]) -> Result<(), PackageRuntimeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error("STATE_WRITE_FAILED"))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error("STATE_WRITE_FAILED"))?;
    file.write_all(content)
        .map_err(io_error("STATE_WRITE_FAILED"))?;
    file.sync_all().map_err(io_error("STATE_WRITE_FAILED"))?;
    fs::rename(&temporary, path).map_err(io_error("STATE_WRITE_FAILED"))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path, code: &str) -> Result<T, PackageRuntimeError> {
    let content = fs::read(path).map_err(|error| {
        PackageRuntimeError::new(code, format!("could not read {}: {error}", path.display()))
    })?;
    serde_json::from_slice(&content).map_err(|error| {
        PackageRuntimeError::new(code, format!("invalid {}: {error}", path.display()))
    })
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, PackageRuntimeError> {
    let mut entries = fs::read_dir(path)
        .map_err(io_error("RUNTIME_STATE_UNAVAILABLE"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error("RUNTIME_STATE_UNAVAILABLE"))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn remove_entry(path: &Path) -> Result<(), PackageRuntimeError> {
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(io_error("CLEANUP_FAILED"))
    } else {
        fs::remove_file(path).map_err(io_error("CLEANUP_FAILED"))
    }
}

fn sync_directory(path: &Path) -> Result<(), PackageRuntimeError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error("STATE_SYNC_FAILED"))
}

fn io_error(code: &'static str) -> impl Fn(std::io::Error) -> PackageRuntimeError {
    move |error| PackageRuntimeError::new(code, error.to_string())
}
