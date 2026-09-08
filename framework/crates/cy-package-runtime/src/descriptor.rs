use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek},
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

use cy_platform_api::normalize_repository_manifest;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{
    ArtifactDigest, CapabilityId, PackageId, PackageInspection, PackageRuntimeError, PackageSource,
    PackageVersion, VerificationEvidence, VerifiedPackage,
};

#[derive(Debug, Deserialize)]
struct PackageDescriptor {
    record_type: String,
    spec_version: String,
    package: DescriptorPackage,
    capability: DescriptorCapability,
    implementation: DescriptorImplementation,
    dependencies: DescriptorDependencies,
    integrity: DescriptorIntegrity,
    publication_status: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorPackage {
    id: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorCapability {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorImplementation {
    artifact: DescriptorArtifact,
    entrypoint: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorArtifact {
    status: String,
    digest: String,
    format: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorDependencies {
    lock: DescriptorLock,
}

#[derive(Debug, Deserialize)]
struct DescriptorLock {
    status: String,
    #[serde(rename = "ref")]
    reference: String,
    digest: String,
}

#[derive(Debug, Deserialize)]
struct DescriptorIntegrity {
    artifact_digest: String,
    archive_digest: String,
}

pub(crate) struct InspectedDescriptor {
    pub inspection: PackageInspection,
    descriptor_bytes: Vec<u8>,
    lock_reference: String,
}

pub(crate) fn inspect_source(
    source: &PackageSource,
) -> Result<InspectedDescriptor, PackageRuntimeError> {
    let descriptor_bytes = std::fs::read(&source.descriptor_path).map_err(|error| {
        PackageRuntimeError::new(
            "DESCRIPTOR_UNAVAILABLE",
            format!(
                "could not read {}: {error}",
                source.descriptor_path.display()
            ),
        )
    })?;
    let descriptor: PackageDescriptor =
        serde_json::from_slice(&descriptor_bytes).map_err(|error| {
            PackageRuntimeError::new(
                "DESCRIPTOR_INVALID",
                format!("invalid package descriptor: {error}"),
            )
        })?;
    validate_descriptor(&descriptor)?;
    Ok(InspectedDescriptor {
        inspection: PackageInspection {
            package_id: PackageId::new(descriptor.package.id)?,
            package_version: PackageVersion::new(descriptor.package.version)?,
            artifact_digest: ArtifactDigest::new(descriptor.implementation.artifact.digest)?,
            archive_digest: ArtifactDigest::new(descriptor.integrity.archive_digest)?,
            dependency_lock_digest: ArtifactDigest::new(descriptor.dependencies.lock.digest)?,
            capabilities: vec![CapabilityId::new(descriptor.capability.id)?],
            entrypoint: descriptor.implementation.entrypoint,
        },
        descriptor_bytes,
        lock_reference: descriptor.dependencies.lock.reference,
    })
}

pub(crate) fn verify_source(
    source: &PackageSource,
) -> Result<VerifiedPackage, PackageRuntimeError> {
    let inspected = inspect_source(source)?;
    let actual_archive_digest = digest_file(&source.archive_path)?;
    if actual_archive_digest != inspected.inspection.archive_digest {
        return Err(PackageRuntimeError::new(
            "ARCHIVE_CORRUPT",
            format!(
                "archive digest mismatch: expected {}, got {}",
                inspected.inspection.archive_digest, actual_archive_digest
            ),
        ));
    }

    let file = File::open(&source.archive_path).map_err(|error| {
        PackageRuntimeError::new(
            "ARCHIVE_UNAVAILABLE",
            format!("could not open {}: {error}", source.archive_path.display()),
        )
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        PackageRuntimeError::new("ARCHIVE_INVALID", format!("invalid ZIP package: {error}"))
    })?;
    let entries = read_verified_entries(&mut archive)?;
    let manifest_bytes = entries.get("plugin.manifest.json").ok_or_else(|| {
        PackageRuntimeError::new("MANIFEST_MISSING", "package has no plugin.manifest.json")
    })?;
    let lock_bytes = entries.get(&inspected.lock_reference).ok_or_else(|| {
        PackageRuntimeError::new(
            "DEPENDENCY_LOCK_MISSING",
            format!("package has no {}", inspected.lock_reference),
        )
    })?;
    let lock_digest = digest_bytes(lock_bytes);
    if lock_digest != inspected.inspection.dependency_lock_digest {
        return Err(PackageRuntimeError::new(
            "DEPENDENCY_LOCK_CORRUPT",
            "dependency lock digest does not match the package descriptor",
        ));
    }
    let artifact_digest = digest_entries(&entries);
    if artifact_digest != inspected.inspection.artifact_digest {
        return Err(PackageRuntimeError::new(
            "ARTIFACT_CORRUPT",
            format!(
                "artifact digest mismatch: expected {}, got {}",
                inspected.inspection.artifact_digest, artifact_digest
            ),
        ));
    }
    let manifest_value = serde_json::from_slice(manifest_bytes).map_err(|error| {
        PackageRuntimeError::new(
            "MANIFEST_INVALID",
            format!("invalid repository plugin manifest: {error}"),
        )
    })?;
    let manifest = normalize_repository_manifest(manifest_value)
        .map_err(|error| PackageRuntimeError::new("MANIFEST_INVALID", error))?;
    if manifest.plugin.id != inspected.inspection.package_id.as_str()
        || manifest.plugin.version != inspected.inspection.package_version.as_str()
    {
        return Err(PackageRuntimeError::new(
            "PACKAGE_IDENTITY_MISMATCH",
            "repository plugin manifest identity does not match package descriptor",
        ));
    }
    let manifest_capabilities = manifest
        .capability_descriptors
        .iter()
        .map(|descriptor| descriptor.id.id.as_str())
        .collect::<BTreeSet<_>>();
    if inspected
        .inspection
        .capabilities
        .iter()
        .any(|capability| !manifest_capabilities.contains(capability.as_str()))
    {
        return Err(PackageRuntimeError::new(
            "PACKAGE_CAPABILITY_MISMATCH",
            "package descriptor capability is absent from repository plugin manifest",
        ));
    }

    let evidence = VerificationEvidence {
        verifier: "cy-package-runtime/0.1".to_string(),
        verified_at_unix_ms: unix_ms(),
        artifact_digest: inspected.inspection.artifact_digest.clone(),
        archive_digest: inspected.inspection.archive_digest.clone(),
        descriptor_digest: digest_bytes(&inspected.descriptor_bytes),
        manifest_digest: digest_bytes(manifest_bytes),
        dependency_lock_digest: lock_digest,
    };
    Ok(VerifiedPackage {
        inspection: inspected.inspection,
        evidence,
        source: source.clone(),
    })
}

pub(crate) fn validate_archive_paths<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<(), PackageRuntimeError> {
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            PackageRuntimeError::new("ARCHIVE_INVALID", format!("invalid ZIP entry: {error}"))
        })?;
        let name = entry.name();
        validate_entry_name(name)?;
        if !names.insert(name.to_string()) {
            return Err(PackageRuntimeError::new(
                "ARCHIVE_DUPLICATE_ENTRY",
                format!("duplicate archive entry: {name}"),
            ));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(PackageRuntimeError::new(
                "ARCHIVE_SYMLINK_REJECTED",
                format!("symbolic link archive entry is forbidden: {name}"),
            ));
        }
    }
    Ok(())
}

fn read_verified_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<BTreeMap<String, Vec<u8>>, PackageRuntimeError> {
    validate_archive_paths(archive)?;
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            PackageRuntimeError::new("ARCHIVE_INVALID", format!("invalid ZIP entry: {error}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).map_err(|error| {
            PackageRuntimeError::new(
                "ARCHIVE_READ_FAILED",
                format!("could not read {name}: {error}"),
            )
        })?;
        entries.insert(name, content);
    }
    Ok(entries)
}

fn validate_descriptor(descriptor: &PackageDescriptor) -> Result<(), PackageRuntimeError> {
    if descriptor.record_type != "package_descriptor"
        || descriptor.spec_version != "0.1"
        || descriptor.publication_status != "PUBLISHED"
        || descriptor.implementation.artifact.status != "PUBLISHED"
        || descriptor.implementation.artifact.format != "zip"
        || descriptor.dependencies.lock.status != "LOCKED"
    {
        return Err(PackageRuntimeError::new(
            "DESCRIPTOR_INVALID",
            "descriptor must be a published Package Spec v0.1 ZIP with a locked dependency set",
        ));
    }
    let artifact = ArtifactDigest::new(descriptor.implementation.artifact.digest.clone())?;
    let integrity_artifact = ArtifactDigest::new(descriptor.integrity.artifact_digest.clone())?;
    if artifact != integrity_artifact {
        return Err(PackageRuntimeError::new(
            "DESCRIPTOR_INVALID",
            "artifact and integrity digests differ",
        ));
    }
    ArtifactDigest::new(descriptor.integrity.archive_digest.clone())?;
    ArtifactDigest::new(descriptor.dependencies.lock.digest.clone())?;
    validate_entry_name(&descriptor.dependencies.lock.reference)?;
    if descriptor.implementation.entrypoint.trim().is_empty() {
        return Err(PackageRuntimeError::new(
            "DESCRIPTOR_INVALID",
            "implementation entrypoint is required",
        ));
    }
    Ok(())
}

fn validate_entry_name(name: &str) -> Result<(), PackageRuntimeError> {
    if name.is_empty()
        || name.contains('\\')
        || name.starts_with('/')
        || name.starts_with('~')
        || name
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'))
    {
        return Err(PackageRuntimeError::new(
            "ARCHIVE_ABSOLUTE_PATH_REJECTED",
            format!("absolute or platform-specific archive path is forbidden: {name}"),
        ));
    }
    let path = Path::new(name);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(PackageRuntimeError::new(
            "ARCHIVE_TRAVERSAL_REJECTED",
            format!("archive path traversal is forbidden: {name}"),
        ));
    }
    Ok(())
}

pub(crate) fn digest_file(path: &Path) -> Result<ArtifactDigest, PackageRuntimeError> {
    let mut file = File::open(path).map_err(|error| {
        PackageRuntimeError::new(
            "ARTIFACT_UNAVAILABLE",
            format!("could not open {}: {error}", path.display()),
        )
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            PackageRuntimeError::new(
                "ARTIFACT_READ_FAILED",
                format!("could not read {}: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(
        ArtifactDigest::new(format!("sha256:{:x}", digest.finalize()))
            .expect("SHA-256 output is valid"),
    )
}

pub(crate) fn digest_bytes(content: &[u8]) -> ArtifactDigest {
    ArtifactDigest::new(format!("sha256:{:x}", Sha256::digest(content)))
        .expect("SHA-256 output is valid")
}

pub(crate) fn digest_entries(entries: &BTreeMap<String, Vec<u8>>) -> ArtifactDigest {
    let mut digest = Sha256::new();
    for (name, content) in entries {
        let name = name.as_bytes();
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name);
        digest.update((content.len() as u64).to_be_bytes());
        digest.update(content);
    }
    ArtifactDigest::new(format!("sha256:{:x}", digest.finalize())).expect("SHA-256 output is valid")
}

pub(crate) fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
