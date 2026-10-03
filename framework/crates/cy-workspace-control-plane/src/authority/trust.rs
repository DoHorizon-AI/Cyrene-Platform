//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Trusted Product snapshot import and provenance verification       │
//! │  Module: cy_workspace_control_plane::authority::trust              │
//! │  Role: Verify owner and Platform attestations before loading data. │
//! │                                                                     │
//! │  模块职责：从受保护信任配置验证owner和Platform证明并加载快照。      │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::snapshot::{now_unix_ms, ContractSnapshot, SnapshotIdentity, VerifiedContractSnapshot};

const DATA_BUNDLE_PROOF_FILENAME: &str = "data-bundle-proof-v1.json";
const PRODUCT_BUNDLE_MANIFEST_FILENAME: &str = "product-contract-bundle.json";
const PRODUCT_POLICY_FILENAME: &str = "workspace-product-policy-v2.json";
const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
const SLSA_PROVENANCE_V1: &str = "https://slsa.dev/provenance/v1";
const MAX_PROOF_BYTES: u64 = 1024 * 1024;
const MAX_ATTESTATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DATA_BUNDLE_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DATA_BUNDLE_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DATA_BUNDLE_MEMBER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DATA_BUNDLE_ENTRIES: usize = 1024;
const MAX_DATA_BUNDLE_PATH_BYTES: usize = 4096;

/// Failure while loading protected trust configuration or verifying a staged artifact.
#[derive(Debug, Error)]
pub enum SnapshotTrustError {
    /// Trust config, proof, artifact, or source identity is invalid.
    #[error("snapshot proof rejected: {0}")]
    Invalid(String),
    /// A required file cannot be read from the protected staging area.
    #[error("snapshot trust input unavailable: {0}")]
    Io(#[from] std::io::Error),
    /// GitHub's offline attestation verifier rejected a proof.
    #[error("GitHub release attestation rejected: {0}")]
    AttestationRejected(String),
}

/// Protected local trust roots for Product owners and Platform authorization policy.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotTrustConfig {
    schema_version: u32,
    wire_api_version: String,
    contract_api_version: String,
    policy_schema_version: String,
    github_cli_path: PathBuf,
    github_trusted_root_path: PathBuf,
    artifact_versions_root: PathBuf,
    owners: Vec<TrustedSourceIdentity>,
    policy_source: TrustedSourceIdentity,
}

/// One source whose repository, release refs and workflow are pinned out of band.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustedSourceIdentity {
    owner_id: Option<String>,
    repository: String,
    allowed_refs: Vec<String>,
    workflow: String,
    certificate_identity: String,
}

/// Paths and expected digest from the already staged, explicitly selected artifact.
pub struct SnapshotArtifactInput<'a> {
    /// Immutable content identity from the verified component release manifest.
    pub artifact_id: &'a str,
    /// Exact downloaded archive whose raw bytes match `artifact_id`.
    pub archive_path: &'a Path,
    /// Directory containing the extracted Product bundle, policy, and proof files.
    pub artifact_root: &'a Path,
    /// Proof path declared and digest-bound by the outer component manifest.
    pub proof_path: &'a str,
    /// SHA-256 declared for the proof file in the outer component manifest.
    pub proof_sha256: &'a str,
    /// Generation from the locally confirmed maintenance plan.
    pub generation: u64,
    /// Epoch from the same confirmed plan; must equal the new generation.
    pub activation_epoch: u64,
}

/// Verifies data-bundle proofs against locally protected repository/workflow identities.
pub struct SnapshotTrust {
    config: SnapshotTrustConfig,
}

impl SnapshotTrust {
    /// Load root-controlled trust policy without following symlinks or accepting writable files.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SnapshotTrustError> {
        let bytes = read_protected_file(path.as_ref(), MAX_PROOF_BYTES)?;
        let config: SnapshotTrustConfig = serde_json::from_slice(&bytes)
            .map_err(|_| SnapshotTrustError::Invalid("trust config JSON is invalid".into()))?;
        validate_trust_config(&config)?;
        Ok(Self { config })
    }

    /// Return the protected immutable `versions/` root used by Authority imports.
    pub fn artifact_versions_root(&self) -> &Path {
        &self.config.artifact_versions_root
    }

    /// Resolve a strict archive digest to its fixed immutable extracted version directory.
    pub fn resolve_artifact_root(&self, artifact_id: &str) -> Result<PathBuf, SnapshotTrustError> {
        let raw_digest = digest_hex(artifact_id)?;
        let candidate = self.config.artifact_versions_root.join(raw_digest);
        canonical_artifact_root(&self.config.artifact_versions_root, &candidate)
    }

    /// Resolve an archive digest to its fixed sibling `archives/` file.
    pub fn resolve_archive_path(&self, artifact_id: &str) -> Result<PathBuf, SnapshotTrustError> {
        let raw_digest = digest_hex(artifact_id)?;
        let versions_root = fs::canonicalize(&self.config.artifact_versions_root)?;
        let parent = versions_root
            .parent()
            .ok_or_else(|| invalid("artifact versions root has no parent"))?;
        let archives_root = parent.join("archives");
        let archives_metadata = fs::symlink_metadata(&archives_root)?;
        if archives_metadata.file_type().is_symlink() || !archives_metadata.is_dir() {
            return Err(invalid("selected artifact archive root is unsafe"));
        }
        reject_group_or_world_writable(&archives_metadata, "artifact archive root")?;
        let archives_root = fs::canonicalize(&archives_root)?;
        let archive = archives_root.join(format!("sha256-{raw_digest}.tar.zst"));
        let metadata = fs::symlink_metadata(&archive)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(invalid("selected artifact archive is not a regular file"));
        }
        reject_group_or_world_writable(&metadata, "selected artifact archive")?;
        let canonical = fs::canonicalize(&archive)?;
        if !canonical.starts_with(&archives_root) {
            return Err(invalid("selected artifact archive escaped the fixed root"));
        }
        Ok(canonical)
    }

    /// Ensure monotonic activation state lives outside immutable artifact-version directories.
    pub fn validate_activation_state_dir(
        &self,
        state_dir: impl AsRef<Path>,
    ) -> Result<(), SnapshotTrustError> {
        let versions_root = fs::canonicalize(&self.config.artifact_versions_root)?;
        let state_dir = state_dir.as_ref();
        let absolute_state_dir = if state_dir.is_absolute() {
            state_dir.to_path_buf()
        } else {
            std::env::current_dir()?.join(state_dir)
        };
        let canonical_state_dir = match fs::symlink_metadata(&absolute_state_dir) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(invalid("activation state path is not a real directory"));
                }
                fs::canonicalize(&absolute_state_dir)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut missing = Vec::new();
                let mut ancestor = absolute_state_dir.clone();
                loop {
                    match fs::symlink_metadata(&ancestor) {
                        Ok(metadata) => {
                            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                                return Err(invalid(
                                    "activation state parent is not a real directory",
                                ));
                            }
                            let mut resolved = fs::canonicalize(&ancestor)?;
                            for component in missing.iter().rev() {
                                resolved.push(component);
                            }
                            break resolved;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            let name = ancestor.file_name().ok_or_else(|| {
                                invalid("activation state directory path is invalid")
                            })?;
                            missing.push(name.to_os_string());
                            if !ancestor.pop() {
                                return Err(invalid("activation state directory path is invalid"));
                            }
                        }
                        Err(error) => return Err(SnapshotTrustError::Io(error)),
                    }
                }
            }
            Err(error) => return Err(SnapshotTrustError::Io(error)),
        };
        if canonical_state_dir.starts_with(&versions_root) {
            return Err(invalid(
                "activation state must be outside immutable artifact versions",
            ));
        }
        Ok(())
    }

    /// Validate a full immutable owner/policy pair and create an activation-only token.
    pub fn verify_and_load(
        &self,
        input: SnapshotArtifactInput<'_>,
    ) -> Result<VerifiedContractSnapshot, SnapshotTrustError> {
        if input.generation == 0
            || input.activation_epoch != input.generation
            || input.proof_path != DATA_BUNDLE_PROOF_FILENAME
            || !is_sha256_prefixed_digest(input.proof_sha256)
            || !input.artifact_id.starts_with("sha256:")
        {
            return Err(invalid(
                "artifact identity or activation generation is invalid",
            ));
        }

        let expected_archive_path = self.resolve_archive_path(input.artifact_id)?;
        if fs::canonicalize(input.archive_path)? != expected_archive_path {
            return Err(invalid(
                "selected archive path differs from the fixed artifact mapping",
            ));
        }
        let content_digest = format!(
            "sha256:{}",
            sha256_file(input.archive_path, MAX_DATA_BUNDLE_ARCHIVE_BYTES)?
        );
        if content_digest != input.artifact_id {
            return Err(invalid(
                "data-bundle archive digest differs from the selected artifact",
            ));
        }

        let root = self.resolve_artifact_root(input.artifact_id)?;
        if fs::canonicalize(input.artifact_root)? != root {
            return Err(invalid(
                "selected version directory differs from the fixed artifact mapping",
            ));
        }
        verify_archive_matches_tree(input.archive_path, &root)?;
        let proof_path = safe_join(&root, input.proof_path)?;
        let proof_bytes = read_regular_file(&proof_path, MAX_PROOF_BYTES)?;
        if format!("sha256:{}", sha256_hex(&proof_bytes)) != input.proof_sha256 {
            return Err(invalid(
                "proof digest differs from the outer component manifest",
            ));
        }
        let proof: DataBundleProof = serde_json::from_slice(&proof_bytes)
            .map_err(|_| invalid("data-bundle proof JSON is invalid"))?;
        validate_proof_header(&proof, &self.config)?;

        let mut source_commits = BTreeMap::new();
        let mut source_pins = BTreeMap::new();
        if proof.owners.len() != self.config.owners.len() {
            return Err(invalid(
                "owner set differs from protected trust configuration",
            ));
        }
        let mut previous_owner: Option<&str> = None;
        for (owner, trusted) in proof.owners.iter().zip(&self.config.owners) {
            if previous_owner.is_some_and(|previous| previous >= owner.owner_id.as_str()) {
                return Err(invalid("Product owner proofs are not sorted"));
            }
            previous_owner = Some(&owner.owner_id);
            verify_owner_proof(&root, owner, trusted, &proof, &self.config)?;
            if source_commits
                .insert(owner.owner_id.clone(), owner.source.commit.clone())
                .is_some()
            {
                return Err(invalid("duplicate Product owner in proof"));
            }
            source_pins.insert(owner.owner_id.clone(), owner.clone());
        }
        if source_commits.is_empty() {
            return Err(invalid("Product owner proof list is empty"));
        }

        verify_policy_proof(&root, &proof, &self.config)?;

        let owner_catalog_digests = source_pins
            .iter()
            .map(|(owner_id, owner)| {
                Ok((
                    owner_id.clone(),
                    digest_hex(&owner.catalog_sha256)?.to_owned(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, SnapshotTrustError>>()?;

        let bundle_manifest_path = safe_join(&root, &proof.manifest_path)?;
        let actual_manifest_sha =
            sha256_hex(&read_regular_file(&bundle_manifest_path, MAX_PROOF_BYTES)?);
        let manifest_digest = digest_hex(&proof.manifest_sha256)?;
        let policy_digest = digest_hex(&proof.policy_sha256)?;
        if actual_manifest_sha != manifest_digest {
            return Err(invalid("Product bundle manifest digest mismatch"));
        }
        verify_bundle_owner_projection(&bundle_manifest_path, &proof, &source_pins)?;

        let policy_path = safe_join(&root, &proof.policy_path)?;
        let policy_bytes = read_regular_file(&policy_path, MAX_PROOF_BYTES)?;
        if sha256_hex(&policy_bytes) != policy_digest {
            return Err(invalid("Platform authorization policy digest mismatch"));
        }

        // Reuse the full reference, file digest, OpenAPI schema, resource, and scope validation.
        let owner_source_shas = source_commits.clone();
        let pins = ProductBundlePins::new(
            proof.protocol_version.clone(),
            manifest_digest.to_owned(),
            owner_source_shas,
            proof.policy_schema_version.clone(),
            policy_digest.to_owned(),
        );
        let bundle = ProductContractBundle::load(&root, &pins)
            .map_err(|error| invalid(format!("Product bundle validation failed: {error}")))?;
        let policy = TrustedProductPolicy::load(&policy_path, &pins)
            .map_err(|error| invalid(format!("Platform policy validation failed: {error}")))?;
        policy
            .validate_bundle(&bundle)
            .map_err(|error| invalid(format!("bundle/policy pair validation failed: {error}")))?;

        let snapshot = ContractSnapshot {
            generation: input.generation,
            activation_epoch: input.activation_epoch,
            bundle: std::sync::Arc::new(bundle),
            policy: std::sync::Arc::new(policy),
            pins,
            identity: SnapshotIdentity {
                wire_api_version: proof.protocol_version,
                bundle_manifest_sha256: manifest_digest.to_owned(),
                policy_sha256: policy_digest.to_owned(),
                content_digest,
                policy_source_commit: proof.policy_source.source.commit,
                source_commits,
                owner_catalog_digests,
            },
            artifact_id: input.artifact_id.to_owned(),
            activated_at_unix_ms: now_unix_ms(),
        };
        Ok(VerifiedContractSnapshot::from_verified(snapshot))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DataBundleProof {
    schema_version: u32,
    protocol_version: String,
    contract_api_version: String,
    manifest_path: String,
    manifest_sha256: String,
    policy_path: String,
    policy_sha256: String,
    policy_schema_version: String,
    owners: Vec<OwnerProof>,
    policy_source: PolicySourceProof,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerProof {
    owner_id: String,
    source: SourceProof,
    catalog_path: String,
    catalog_sha256: String,
    provenance: SourceProvenance,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicySourceProof {
    source: SourceProof,
    path: String,
    sha256: String,
    provenance: SourceProvenance,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceProof {
    repository: String,
    #[serde(rename = "ref")]
    git_ref: String,
    commit: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceProvenance {
    attestation: AttestationWrapper,
    bundle_path: String,
    bundle_sha256: String,
    subject_path: String,
    subject_digest: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttestationWrapper {
    attestation: AttestationSummary,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttestationSummary {
    kind: String,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    uri: Option<String>,
    subject_name: String,
    repository: String,
    workflow: String,
    predicate_type: String,
    run: AttestationRun,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttestationRun {
    id: String,
    attempt: u32,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleManifest {
    format_version: u32,
    wire_api_version: String,
    owners: Vec<BundleOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleOwner {
    owner_id: String,
    repository: String,
    source_sha: String,
    catalog_path: String,
    catalog_sha256: String,
}

fn validate_trust_config(config: &SnapshotTrustConfig) -> Result<(), SnapshotTrustError> {
    if config.schema_version != 1
        || config.wire_api_version.trim().is_empty()
        || config.contract_api_version.trim().is_empty()
        || config.policy_schema_version.trim().is_empty()
        || config.owners.is_empty()
        || config.github_cli_path.as_os_str().is_empty()
        || config.github_trusted_root_path.as_os_str().is_empty()
        || config.artifact_versions_root.as_os_str().is_empty()
    {
        return Err(invalid("trust config is incomplete"));
    }
    validate_source_identity(&config.policy_source, true)?;
    let mut owner_ids = BTreeSet::new();
    let mut repositories = BTreeSet::new();
    let mut previous_owner: Option<&str> = None;
    for owner in &config.owners {
        let owner_id = owner
            .owner_id
            .as_deref()
            .ok_or_else(|| invalid("Product trust identity is missing ownerId"))?;
        if previous_owner.is_some_and(|previous| previous >= owner_id)
            || !valid_owner_id(owner_id)
            || !owner_ids.insert(owner_id)
            || !repositories.insert(normalized_repository(&owner.repository))
        {
            return Err(invalid("duplicate or malformed Product trust identity"));
        }
        previous_owner = Some(owner_id);
        validate_source_identity(owner, false)?;
    }
    if repositories.contains(&normalized_repository(&config.policy_source.repository)) {
        return Err(invalid(
            "Platform policy trust identity must be separate from Product owners",
        ));
    }
    validate_protected_regular_file(&config.github_cli_path)?;
    read_protected_file(&config.github_trusted_root_path, 64 * 1024 * 1024)?;
    let root = fs::canonicalize(&config.artifact_versions_root)?;
    let metadata = fs::symlink_metadata(&config.artifact_versions_root)?;
    if metadata.file_type().is_symlink() || !root.is_dir() {
        return Err(invalid(
            "configured artifact versions root is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(invalid(
                "artifact versions root is writable by group or others",
            ));
        }
    }
    Ok(())
}

fn validate_source_identity(
    identity: &TrustedSourceIdentity,
    is_policy: bool,
) -> Result<(), SnapshotTrustError> {
    if identity.repository.trim().is_empty()
        || identity.allowed_refs.is_empty()
        || identity.workflow.trim().is_empty()
        || identity.certificate_identity.trim().is_empty()
        || identity
            .allowed_refs
            .iter()
            .any(|value| !value.starts_with("refs/heads/") && !value.starts_with("refs/tags/"))
        || (is_policy && identity.owner_id.is_some())
        || (!is_policy && identity.owner_id.is_none())
    {
        return Err(invalid("trusted source identity is malformed"));
    }
    Ok(())
}

fn validate_proof_header(
    proof: &DataBundleProof,
    config: &SnapshotTrustConfig,
) -> Result<(), SnapshotTrustError> {
    if proof.schema_version != 1
        || proof.protocol_version != config.wire_api_version
        || proof.contract_api_version != config.contract_api_version
        || proof.policy_schema_version != config.policy_schema_version
        || !is_sha256_prefixed_digest(&proof.manifest_sha256)
        || !is_sha256_prefixed_digest(&proof.policy_sha256)
        || proof.owners.len() != config.owners.len()
        || proof.manifest_path != PRODUCT_BUNDLE_MANIFEST_FILENAME
        || proof.policy_path != PRODUCT_POLICY_FILENAME
    {
        return Err(invalid(
            "data-bundle proof header does not match protected trust roots",
        ));
    }
    validate_relative_path(&proof.manifest_path)?;
    validate_relative_path(&proof.policy_path)?;
    Ok(())
}

fn verify_owner_proof(
    root: &Path,
    owner: &OwnerProof,
    trust: &TrustedSourceIdentity,
    proof: &DataBundleProof,
    config: &SnapshotTrustConfig,
) -> Result<(), SnapshotTrustError> {
    let expected_owner = trust.owner_id.as_deref().unwrap_or_default();
    if owner.owner_id != expected_owner
        || normalized_repository(&owner.source.repository)
            != normalized_repository(&trust.repository)
        || !trust.allowed_refs.contains(&owner.source.git_ref)
        || !is_git_commit(&owner.source.commit)
        || !is_sha256_prefixed_digest(&owner.catalog_sha256)
        || owner.provenance.subject_path != owner.catalog_path
        || owner.provenance.attestation.attestation.subject_name != owner.catalog_path
        || owner.provenance.subject_digest != owner.catalog_sha256
        || normalized_repository(&owner.provenance.attestation.attestation.repository)
            != normalized_repository(&trust.repository)
        || owner.provenance.attestation.attestation.workflow != trust.workflow
    {
        return Err(invalid(format!(
            "owner provenance identity mismatch: {expected_owner}"
        )));
    }
    validate_attestation_summary(
        &owner.provenance.attestation.attestation,
        &owner.source,
        trust,
    )?;
    verify_local_subject(
        root,
        &owner.catalog_path,
        digest_hex(&owner.catalog_sha256)?,
    )?;
    verify_attestation_bundle(root, &owner.provenance, config)?;
    verify_with_github_cli(
        root,
        &owner.catalog_path,
        &owner.provenance,
        &owner.source,
        trust,
        config,
    )?;

    // The same catalog digest is bound in both the owner attestation and bundle manifest.
    if owner.catalog_path.is_empty() || proof.manifest_path.is_empty() {
        return Err(invalid("owner catalog path is empty"));
    }
    Ok(())
}

fn verify_policy_proof(
    root: &Path,
    proof: &DataBundleProof,
    config: &SnapshotTrustConfig,
) -> Result<(), SnapshotTrustError> {
    let policy = &proof.policy_source;
    let trust = &config.policy_source;
    if normalized_repository(&policy.source.repository) != normalized_repository(&trust.repository)
        || !trust.allowed_refs.contains(&policy.source.git_ref)
        || !is_git_commit(&policy.source.commit)
        || policy.path != proof.policy_path
        || policy.sha256 != proof.policy_sha256
        || policy.provenance.subject_path != policy.path
        || policy.provenance.attestation.attestation.subject_name != policy.path
        || policy.provenance.subject_digest != policy.sha256
        || normalized_repository(&policy.provenance.attestation.attestation.repository)
            != normalized_repository(&trust.repository)
        || policy.provenance.attestation.attestation.workflow != trust.workflow
    {
        return Err(invalid("Platform policy provenance identity mismatch"));
    }
    validate_attestation_summary(
        &policy.provenance.attestation.attestation,
        &policy.source,
        trust,
    )?;
    verify_local_subject(root, &policy.path, digest_hex(&policy.sha256)?)?;
    verify_attestation_bundle(root, &policy.provenance, config)?;
    verify_with_github_cli(
        root,
        &policy.path,
        &policy.provenance,
        &policy.source,
        trust,
        config,
    )
}

fn validate_attestation_summary(
    attestation: &AttestationSummary,
    source: &SourceProof,
    trust: &TrustedSourceIdentity,
) -> Result<(), SnapshotTrustError> {
    if attestation.kind != "github-artifact-attestation"
        || attestation.predicate_type != SLSA_PROVENANCE_V1
        || normalized_repository(&attestation.repository)
            != normalized_repository(&trust.repository)
        || attestation.workflow != trust.workflow
        || attestation
            .uri
            .as_deref()
            .is_some_and(|uri| !uri.starts_with("https://github.com/"))
        || attestation.subject_name.trim().is_empty()
        || attestation.run.id.is_empty()
        || attestation.run.attempt == 0
        || !attestation.run.url.starts_with("https://github.com/")
        || !is_git_commit(&source.commit)
    {
        return Err(invalid("release attestation metadata is malformed"));
    }
    let _ = trust.certificate_identity.as_str();
    Ok(())
}

fn verify_local_subject(
    root: &Path,
    subject_relative_path: &str,
    expected_sha256: &str,
) -> Result<(), SnapshotTrustError> {
    validate_relative_path(subject_relative_path)?;
    if !is_sha256_hex(expected_sha256) {
        return Err(invalid("attested subject digest is invalid"));
    }
    let path = safe_join(root, subject_relative_path)?;
    let bytes = read_regular_file(&path, 64 * 1024 * 1024)?;
    if sha256_hex(&bytes) != expected_sha256 {
        return Err(invalid("attested source subject digest mismatch"));
    }
    Ok(())
}

fn verify_attestation_bundle(
    root: &Path,
    provenance: &SourceProvenance,
    config: &SnapshotTrustConfig,
) -> Result<(), SnapshotTrustError> {
    validate_relative_path(&provenance.bundle_path)?;
    if !is_sha256_prefixed_digest(&provenance.bundle_sha256) {
        return Err(invalid("attestation bundle digest is invalid"));
    }
    let bundle_path = safe_join(root, &provenance.bundle_path)?;
    let bytes = read_regular_file(&bundle_path, MAX_ATTESTATION_BYTES)?;
    if format!("sha256:{}", sha256_hex(&bytes)) != provenance.bundle_sha256 {
        return Err(invalid("attestation bundle digest mismatch"));
    }
    let root_metadata = fs::symlink_metadata(&config.github_trusted_root_path)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_file() {
        return Err(invalid("GitHub custom trusted root is unsafe"));
    }
    Ok(())
}

fn verify_with_github_cli(
    artifact_root: &Path,
    subject_relative_path: &str,
    provenance: &SourceProvenance,
    source: &SourceProof,
    trust: &TrustedSourceIdentity,
    config: &SnapshotTrustConfig,
) -> Result<(), SnapshotTrustError> {
    let subject_path = safe_join(artifact_root, subject_relative_path)?;
    let bundle_path = safe_join(artifact_root, &provenance.bundle_path)?;
    let github_cli_path = fs::canonicalize(&config.github_cli_path)?;
    let output = Command::new(github_cli_path)
        .arg("attestation")
        .arg("verify")
        .arg(&subject_path)
        .arg("--repo")
        .arg(&trust.repository)
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--custom-trusted-root")
        .arg(&config.github_trusted_root_path)
        .arg("--cert-identity")
        .arg(&trust.certificate_identity)
        .arg("--cert-oidc-issuer")
        .arg(GITHUB_OIDC_ISSUER)
        .arg("--signer-workflow")
        .arg(&trust.workflow)
        .arg("--source-digest")
        .arg(format!("sha1:{}", source.commit))
        .arg("--source-ref")
        .arg(&source.git_ref)
        .arg("--predicate-type")
        .arg(SLSA_PROVENANCE_V1)
        .arg("--format=json")
        .output()?;
    if !output.status.success() {
        return Err(SnapshotTrustError::AttestationRejected(format!(
            "{} for {}",
            String::from_utf8_lossy(&output.stderr).trim(),
            source.repository
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| invalid("GitHub attestation verifier returned invalid JSON"))?;
    let strings = collect_json_strings(&value);
    let raw_subject_digest = provenance
        .subject_digest
        .strip_prefix("sha256:")
        .unwrap_or(&provenance.subject_digest);
    let expected_strings = [
        source.git_ref.as_str(),
        source.commit.as_str(),
        trust.workflow.as_str(),
        trust.certificate_identity.as_str(),
        GITHUB_OIDC_ISSUER,
        raw_subject_digest,
    ];
    let source_digest = format!("sha1:{}", source.commit);
    if !strings
        .iter()
        .any(|actual| normalized_repository(actual) == normalized_repository(&source.repository))
        || expected_strings.iter().any(|expected| {
            !strings.contains(expected)
                && (*expected != source.commit || !strings.contains(&source_digest.as_str()))
        })
    {
        return Err(invalid(
            "verified attestation output does not bind expected source identity",
        ));
    }
    Ok(())
}

fn verify_bundle_owner_projection(
    manifest_path: &Path,
    proof: &DataBundleProof,
    source_pins: &BTreeMap<String, OwnerProof>,
) -> Result<(), SnapshotTrustError> {
    let bytes = read_regular_file(manifest_path, MAX_PROOF_BYTES)?;
    let manifest: BundleManifest = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("Product bundle manifest JSON is invalid"))?;
    if manifest.format_version != 2
        || manifest.wire_api_version != proof.protocol_version
        || manifest.owners.len() != proof.owners.len()
    {
        return Err(invalid(
            "Product bundle owner projection differs from proof",
        ));
    }
    let mut seen = BTreeSet::new();
    for owner in manifest.owners {
        let proof_owner = source_pins
            .get(&owner.owner_id)
            .ok_or_else(|| invalid("bundle owner missing from owner proof list"))?;
        let repository_short = proof_owner
            .source
            .repository
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if owner.repository != repository_short
            || owner.source_sha != proof_owner.source.commit
            || owner.catalog_path != proof_owner.catalog_path
            || owner.catalog_sha256 != digest_hex(&proof_owner.catalog_sha256)?
            || !seen.insert(owner.owner_id)
        {
            return Err(invalid(
                "Product bundle owner source/digest differs from attested proof",
            ));
        }
    }
    Ok(())
}

fn canonical_artifact_root(
    configured_root: &Path,
    artifact_root: &Path,
) -> Result<PathBuf, SnapshotTrustError> {
    let configured = fs::canonicalize(configured_root)?;
    let root_metadata = fs::symlink_metadata(artifact_root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(invalid("artifact root is not a real directory"));
    }
    reject_group_or_world_writable(&root_metadata, "artifact root")?;
    let root = fs::canonicalize(artifact_root)?;
    if !root.starts_with(&configured) || root == configured {
        return Err(invalid(
            "artifact root is outside the protected version directory",
        ));
    }
    Ok(root)
}

fn verify_archive_matches_tree(
    archive_path: &Path,
    artifact_root: &Path,
) -> Result<(), SnapshotTrustError> {
    let archive_file = File::open(archive_path)?;
    let decoder = zstd::stream::read::Decoder::new(archive_file)?;
    let bounded_decoder = decoder.take(MAX_DATA_BUNDLE_UNCOMPRESSED_BYTES + 1);
    let mut archive = tar::Archive::new(bounded_decoder);
    let mut archive_paths = BTreeMap::<String, bool>::new();
    let mut total_member_bytes = 0_u64;
    let mut entry_count = 0_usize;

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| invalid("data-bundle entry count overflow"))?;
        if entry_count > MAX_DATA_BUNDLE_ENTRIES {
            return Err(invalid("data-bundle has too many archive entries"));
        }
        let entry_type = entry.header().entry_type();
        let is_directory = entry_type.is_dir();
        if !is_directory && entry_type != tar::EntryType::Regular {
            return Err(invalid(
                "data-bundle archive contains a link or special member",
            ));
        }
        let mode = entry
            .header()
            .mode()
            .map_err(|_| invalid("data-bundle archive member mode is invalid"))?;
        if mode & 0o7022 != 0 {
            return Err(invalid("data-bundle archive member has unsafe permissions"));
        }
        let raw_path = entry.path_bytes();
        let raw_path = std::str::from_utf8(&raw_path)
            .map_err(|_| invalid("data-bundle archive path is not UTF-8"))?;
        let relative_path = normalize_archive_member_path(raw_path, is_directory)?;
        if archive_paths
            .insert(relative_path.clone(), is_directory)
            .is_some()
        {
            return Err(invalid("data-bundle archive contains duplicate paths"));
        }
        let member_size = entry.size();
        if is_directory {
            if member_size != 0 {
                return Err(invalid("data-bundle directory member has content"));
            }
            continue;
        }
        if member_size > MAX_DATA_BUNDLE_MEMBER_BYTES {
            return Err(invalid("data-bundle archive member exceeds the size limit"));
        }
        total_member_bytes = total_member_bytes
            .checked_add(member_size)
            .ok_or_else(|| invalid("data-bundle expanded size overflow"))?;
        if total_member_bytes > MAX_DATA_BUNDLE_UNCOMPRESSED_BYTES {
            return Err(invalid("data-bundle expanded size exceeds the limit"));
        }

        let local_path = safe_join(artifact_root, &relative_path)?;
        let local_metadata = fs::symlink_metadata(&local_path)?;
        if local_metadata.len() != member_size {
            return Err(invalid(
                "data-bundle member size differs from extracted tree",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if local_metadata.nlink() != 1 {
                return Err(invalid("extracted data-bundle member is hard-linked"));
            }
        }
        let mut local_file = File::open(local_path)?;
        let mut archive_hasher = Sha256::new();
        let mut local_hasher = Sha256::new();
        let mut remaining = member_size;
        let mut archive_buffer = [0_u8; 64 * 1024];
        let mut local_buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let chunk_size = usize::try_from(remaining.min(archive_buffer.len() as u64))
                .map_err(|_| invalid("data-bundle member size is invalid"))?;
            entry.read_exact(&mut archive_buffer[..chunk_size])?;
            local_file.read_exact(&mut local_buffer[..chunk_size])?;
            let archive_chunk = &archive_buffer[..chunk_size];
            let local_chunk = &local_buffer[..chunk_size];
            if archive_chunk != local_chunk {
                return Err(invalid(
                    "data-bundle member bytes differ from extracted tree",
                ));
            }
            archive_hasher.update(archive_chunk);
            local_hasher.update(local_chunk);
            remaining -= chunk_size as u64;
        }
        if archive_hasher.finalize() != local_hasher.finalize() {
            return Err(invalid(
                "data-bundle member digest differs from extracted tree",
            ));
        }
    }

    let mut decompressed = archive.into_inner();
    let mut drain_buffer = [0_u8; 64 * 1024];
    loop {
        let bytes_read = decompressed.read(&mut drain_buffer)?;
        if bytes_read == 0 {
            break;
        }
        if drain_buffer[..bytes_read].iter().any(|byte| *byte != 0) {
            return Err(invalid(
                "data-bundle archive contains non-padding bytes after the tar terminator",
            ));
        }
    }
    if decompressed.limit() == 0 {
        return Err(invalid("data-bundle decompressed size exceeds the limit"));
    }

    let mut archive_tree = archive_paths;
    let file_paths = archive_tree
        .iter()
        .filter_map(|(path, is_directory)| (!*is_directory).then_some(path.clone()))
        .collect::<Vec<_>>();
    for file_path in file_paths {
        let mut parent = Path::new(&file_path).parent();
        while let Some(parent_path) = parent {
            if parent_path.as_os_str().is_empty() {
                break;
            }
            let parent_name = parent_path
                .to_str()
                .ok_or_else(|| invalid("data-bundle parent path is not UTF-8"))?;
            match archive_tree.get(parent_name) {
                Some(false) => return Err(invalid("data-bundle file is also used as a directory")),
                Some(true) => {}
                None => {
                    archive_tree.insert(parent_name.to_owned(), true);
                }
            }
            parent = parent_path.parent();
        }
    }

    let extracted_tree = collect_extracted_tree(artifact_root)?;
    if archive_tree != extracted_tree {
        return Err(invalid(
            "data-bundle archive inventory differs from extracted version directory",
        ));
    }
    Ok(())
}

fn normalize_archive_member_path(
    path: &str,
    is_directory: bool,
) -> Result<String, SnapshotTrustError> {
    let normalized = if is_directory {
        path.trim_end_matches('/')
    } else {
        path
    };
    if normalized.len() > MAX_DATA_BUNDLE_PATH_BYTES {
        return Err(invalid("data-bundle archive path exceeds the size limit"));
    }
    validate_relative_path(normalized)?;
    if normalized
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid("data-bundle archive path is not canonical"));
    }
    Ok(normalized.to_owned())
}

fn collect_extracted_tree(root: &Path) -> Result<BTreeMap<String, bool>, SnapshotTrustError> {
    let mut tree = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total_file_bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry_result in fs::read_dir(&directory)? {
            let entry = entry_result?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("extracted data-bundle tree contains a symlink"));
            }
            reject_group_or_world_writable(&metadata, "extracted data-bundle path")?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| invalid("extracted data-bundle path escapes its root"))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| invalid("extracted data-bundle path is not UTF-8"))?;
            validate_relative_path(relative)?;
            let is_directory = metadata.is_dir();
            if !is_directory && !metadata.is_file() {
                return Err(invalid(
                    "extracted data-bundle tree contains a special file",
                ));
            }
            if !is_directory {
                if metadata.len() > MAX_DATA_BUNDLE_MEMBER_BYTES {
                    return Err(invalid(
                        "extracted data-bundle member exceeds the size limit",
                    ));
                }
                total_file_bytes = total_file_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| invalid("extracted data-bundle size overflow"))?;
                if total_file_bytes > MAX_DATA_BUNDLE_UNCOMPRESSED_BYTES {
                    return Err(invalid("extracted data-bundle size exceeds the limit"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() != 1 {
                        return Err(invalid("extracted data-bundle member is hard-linked"));
                    }
                }
            }
            if tree.insert(relative.to_owned(), is_directory).is_some()
                || tree.len() > MAX_DATA_BUNDLE_ENTRIES
            {
                return Err(invalid(
                    "extracted data-bundle tree has duplicate/excess entries",
                ));
            }
            if is_directory {
                pending.push(path);
            }
        }
    }
    Ok(tree)
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, SnapshotTrustError> {
    validate_relative_path(relative)?;
    let path = root.join(relative);
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("snapshot input path contains a symlink"));
        }
        reject_group_or_world_writable(&metadata, "snapshot input path")?;
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid("snapshot input is not a regular file"));
    }
    let canonical = fs::canonicalize(&path)?;
    if !canonical.starts_with(root) {
        return Err(invalid("snapshot input escapes the artifact directory"));
    }
    Ok(canonical)
}

fn validate_relative_path(value: &str) -> Result<(), SnapshotTrustError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || value.contains('\\')
    {
        return Err(invalid("snapshot path is not a safe relative path"));
    }
    Ok(())
}

fn read_protected_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, SnapshotTrustError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(invalid(
            "protected trust input is not a bounded regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(invalid(
                "protected trust input is writable by group or others",
            ));
        }
    }
    Ok(fs::read(path)?)
}

fn validate_protected_regular_file(path: &Path) -> Result<(), SnapshotTrustError> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(canonical)?;
    if !metadata.is_file() {
        return Err(invalid("protected trust tool is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(invalid(
                "protected trust tool is writable by group or others",
            ));
        }
    }
    Ok(())
}

fn read_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, SnapshotTrustError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(invalid("snapshot input is not a bounded regular file"));
    }
    reject_group_or_world_writable(&metadata, "snapshot input")?;
    Ok(fs::read(path)?)
}

fn sha256_file(path: &Path, max_bytes: u64) -> Result<String, SnapshotTrustError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(invalid("data-bundle archive is not a bounded regular file"));
    }
    reject_group_or_world_writable(&metadata, "data-bundle archive")?;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| invalid("data-bundle archive size overflow"))?;
        if total > max_bytes {
            return Err(invalid("data-bundle archive exceeds the size limit"));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn reject_group_or_world_writable(
    metadata: &fs::Metadata,
    label: &str,
) -> Result<(), SnapshotTrustError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o7022 != 0 {
            return Err(invalid(format!("{label} has unsafe permissions")));
        }
    }
    let _ = (metadata, label);
    Ok(())
}

fn normalized_repository(value: &str) -> String {
    value
        .trim()
        .strip_prefix("https://github.com/")
        .unwrap_or(value.trim())
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn collect_json_strings(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(value) => vec![value],
        serde_json::Value::Array(values) => values.iter().flat_map(collect_json_strings).collect(),
        serde_json::Value::Object(values) => {
            values.values().flat_map(collect_json_strings).collect()
        }
        _ => Vec::new(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_sha256_prefixed_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256_hex)
}

fn digest_hex(value: &str) -> Result<&str, SnapshotTrustError> {
    value
        .strip_prefix("sha256:")
        .filter(|digest| is_sha256_hex(digest))
        .ok_or_else(|| invalid("digest must use sha256:<lowercase hex> encoding"))
}

fn is_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn invalid(message: impl Into<String>) -> SnapshotTrustError {
    SnapshotTrustError::Invalid(message.into())
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_tar_zst(path: &Path, member_name: &str, contents: &[u8]) {
        let mut tar_bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, member_name, contents)
                .expect("append tar member");
            archive.finish().expect("finish tar archive");
        }
        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), 0).expect("create zstd encoder");
        encoder.write_all(&tar_bytes).expect("compress tar archive");
        fs::write(path, encoder.finish().expect("finish zstd archive"))
            .expect("write archive file");
    }

    fn write_tar_zst_symlink(path: &Path, member_name: &str, target: &str) {
        let mut tar_bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name(target).expect("set symlink target");
            header.set_cksum();
            archive
                .append_data(&mut header, member_name, std::io::empty())
                .expect("append symlink member");
            archive.finish().expect("finish tar archive");
        }
        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), 0).expect("create zstd encoder");
        encoder.write_all(&tar_bytes).expect("compress tar archive");
        fs::write(path, encoder.finish().expect("finish zstd archive"))
            .expect("write archive file");
    }

    fn write_read_only_file(path: &Path, contents: &[u8]) {
        fs::write(path, contents).expect("write test member");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))
                .expect("set test member permissions");
        }
    }

    #[test]
    fn parses_frozen_owner_publication_attestation_wrapper_without_optional_uri() {
        let proof: OwnerProof = serde_json::from_value(serde_json::json!({
            "ownerId": "workspace",
            "source": {
                "repository": "cyrene/workspace",
                "ref": "refs/tags/product-contract-v2-preview-0123456789abcdef0123456789abcdef01234567",
                "commit": "0123456789abcdef0123456789abcdef01234567"
            },
            "catalogPath": "cyrene/workspace/contracts/product/v2/catalog.json",
            "catalogSha256": format!("sha256:{}", "a".repeat(64)),
            "provenance": {
                "attestation": {
                    "attestation": {
                        "kind": "github-artifact-attestation",
                        "subjectName": "cyrene/workspace/contracts/product/v2/catalog.json",
                        "repository": "cyrene/workspace",
                        "workflow": "cyrene/workspace/.github/workflows/product-contract.yml",
                        "predicateType": "https://slsa.dev/provenance/v1",
                        "run": {"id": "123", "attempt": 1, "url": "https://github.com/run/123"}
                    }
                },
                "bundlePath": "attestations/owners/workspace.jsonl",
                "bundleSha256": format!("sha256:{}", "b".repeat(64)),
                "subjectPath": "cyrene/workspace/contracts/product/v2/catalog.json",
                "subjectDigest": format!("sha256:{}", "a".repeat(64))
            }
        }))
        .expect("frozen owner proof parses");

        assert_eq!(proof.owner_id, "workspace");
        assert_eq!(proof.provenance.attestation.attestation.uri, None);
        assert_eq!(
            proof.provenance.attestation.attestation.subject_name,
            proof.catalog_path
        );
    }

    #[test]
    fn rejects_null_optional_attestation_uri() {
        let attestation = serde_json::json!({
            "kind": "github-artifact-attestation",
            "uri": null,
            "subjectName": "catalog.json",
            "repository": "cyrene/workspace",
            "workflow": "workflow.yml",
            "predicateType": "https://slsa.dev/provenance/v1",
            "run": {"id": "123", "attempt": 1, "url": "https://github.com/run/123"}
        });
        assert!(serde_json::from_value::<AttestationSummary>(attestation).is_err());
    }

    #[test]
    fn archive_members_must_match_the_complete_extracted_tree() {
        let temp = TempDir::new().expect("temp directory");
        let archive_path = temp.path().join("bundle.tar.zst");
        let artifact_root = temp.path().join("version");
        fs::create_dir(&artifact_root).expect("artifact root");
        let expected = b"verified catalog and policy payload";
        write_read_only_file(&artifact_root.join("catalog.json"), expected);
        write_tar_zst(&archive_path, "catalog.json", expected);
        verify_archive_matches_tree(&archive_path, &artifact_root)
            .expect("matching archive and tree");

        write_tar_zst(&archive_path, "catalog.json", b"different archive bytes");
        assert!(verify_archive_matches_tree(&archive_path, &artifact_root).is_err());
    }

    #[test]
    fn archive_comparison_rejects_non_padding_data_after_tar_terminator() {
        let temp = TempDir::new().expect("temp directory");
        let archive_path = temp.path().join("bundle.tar.zst");
        let artifact_root = temp.path().join("version");
        fs::create_dir(&artifact_root).expect("artifact root");
        write_read_only_file(&artifact_root.join("catalog.json"), b"catalog");

        let mut tar_bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(7);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "catalog.json", &b"catalog"[..])
                .expect("append tar member");
            archive.finish().expect("finish tar archive");
        }
        tar_bytes.extend_from_slice(b"hidden trailing payload");
        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), 0).expect("create zstd encoder");
        encoder.write_all(&tar_bytes).expect("compress tar archive");
        fs::write(
            &archive_path,
            encoder.finish().expect("finish zstd archive"),
        )
        .expect("write archive file");

        assert!(verify_archive_matches_tree(&archive_path, &artifact_root).is_err());
    }

    #[test]
    fn archive_comparison_rejects_extra_extracted_files_and_symlink_members() {
        let temp = TempDir::new().expect("temp directory");
        let archive_path = temp.path().join("bundle.tar.zst");
        let artifact_root = temp.path().join("version");
        fs::create_dir(&artifact_root).expect("artifact root");
        write_read_only_file(&artifact_root.join("catalog.json"), b"catalog");
        write_read_only_file(&artifact_root.join("unlisted.txt"), b"extra");
        write_tar_zst(&archive_path, "catalog.json", b"catalog");
        assert!(verify_archive_matches_tree(&archive_path, &artifact_root).is_err());

        fs::remove_file(artifact_root.join("unlisted.txt")).expect("remove extra file");
        write_tar_zst_symlink(&archive_path, "catalog-link", "catalog.json");
        assert!(verify_archive_matches_tree(&archive_path, &artifact_root).is_err());
    }
}
