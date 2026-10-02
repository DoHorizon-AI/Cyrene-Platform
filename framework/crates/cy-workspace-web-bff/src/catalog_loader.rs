//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Authority-backed Product catalog view loader                      │
//! │  Module: cy_workspace_web_bff::catalog_loader                       │
//! │  Role: Verify local schemas against an authenticated Authority view. │
//! │                                                                     │
//! │  模块职责：按Authority认证版本加载本地合同，用于BFF保留schema检查。  │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cy_proto::cyrene::workspace::authority::v2::CatalogSnapshotResponse;
use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy, CONTRACT_API_VERSION,
};
use serde::Deserialize;

use crate::product::{ProductCatalogError, ProductOperationCatalog};

/// Product contract manifest stored in an immutable version directory.
pub const CONTRACT_BUNDLE_MANIFEST_FILENAME: &str = "product-contract-bundle.json";

/// Separately approved Platform authorization policy filename.
pub const PRODUCT_POLICY_BUNDLE_FILENAME: &str = "workspace-product-policy-v2.json";

/// Environment variable naming the shared immutable Product bundle versions root.
pub const PRODUCT_BUNDLE_VERSIONS_ROOT_ENV: &str = "CYRENE_WORKSPACE_PRODUCT_BUNDLE_VERSIONS_ROOT";

/// Compatibility constant; its value now names the versions root, not one active bundle.
pub const PRODUCT_CONTRACT_ROOT_ENV: &str = PRODUCT_BUNDLE_VERSIONS_ROOT_ENV;

const PRODUCT_WIRE_API_VERSION: &str = "cyrene.workspace.product.v2";
const PRODUCT_POLICY_SCHEMA_VERSION: &str = "cyrene.workspace.product.authorization-policy.v2";
const MAX_BUNDLE_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Load one immutable local contract pair after matching it to Authority's authenticated view.
///
/// The response is expected to come from `GetCatalogSnapshot` over the authenticated Authority
/// channel. This function never treats request-supplied digests or the local bundle itself as a
/// trust source. Its authenticated archive ID selects `versions/<raw archive SHA>` under the
/// configured versions root, so product and policy files always come from the same immutable
/// artifact that Authority activated. It reuses the full Product contract loader for file digests, references,
/// request/response schemas, resource rules, and scope bindings.
pub fn load_product_operation_catalog_from_authority_snapshot(
    contract_root: impl AsRef<Path>,
    view: &CatalogSnapshotResponse,
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    if view.workspace_id.trim().is_empty()
        || view.organization_id.trim().is_empty()
        || view.catalog_generation == 0
        || view.contract_activation_generation == 0
        || view.catalog_generation != view.contract_activation_generation
        || view.wire_api_version != PRODUCT_WIRE_API_VERSION
        || view.contract_api_version != CONTRACT_API_VERSION
        || view.policy_schema_version != PRODUCT_POLICY_SCHEMA_VERSION
        || !is_sha256_hex(&view.bundle_manifest_sha256)
        || !is_sha256_hex(&view.policy_digest_sha256)
        || view.owners.is_empty()
    {
        return Err(ProductCatalogError::ContractBundle);
    }

    let mut source_commits = BTreeMap::new();
    let mut authority_grants = BTreeSet::new();
    for owner in &view.owners {
        if owner.owner_id.trim().is_empty()
            || owner.component_id.trim().is_empty()
            || !is_git_commit(&owner.source_commit)
        {
            return Err(ProductCatalogError::ContractBundle);
        }
        if source_commits
            .insert(owner.owner_id.clone(), owner.source_commit.clone())
            .is_some()
        {
            return Err(ProductCatalogError::ContractBundle);
        }
        for operation_id in &owner.granted_operation_ids {
            if operation_id.trim().is_empty()
                || !authority_grants.insert((owner.owner_id.clone(), operation_id.clone()))
            {
                return Err(ProductCatalogError::TrustedPolicy);
            }
        }
    }

    let versions_root = contract_root
        .as_ref()
        .canonicalize()
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if !versions_root.is_dir() || !is_sha256_prefixed(&view.artifact_id) {
        return Err(ProductCatalogError::ContractBundle);
    }
    let artifact_sha = view
        .artifact_id
        .strip_prefix("sha256:")
        .ok_or(ProductCatalogError::ContractBundle)?;
    let artifact_dir = versions_root.join(artifact_sha);
    let artifact_metadata = std::fs::symlink_metadata(&artifact_dir)
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if artifact_metadata.file_type().is_symlink() || !artifact_metadata.is_dir() {
        return Err(ProductCatalogError::ContractBundle);
    }
    let root = artifact_dir
        .canonicalize()
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if !root.starts_with(&versions_root) || root == versions_root {
        return Err(ProductCatalogError::ContractBundle);
    }
    validate_owner_projection(&root, view)?;
    let pins = ProductBundlePins::new(
        view.wire_api_version.clone(),
        view.bundle_manifest_sha256.clone(),
        source_commits,
        view.policy_schema_version.clone(),
        view.policy_digest_sha256.clone(),
    );
    let bundle = ProductContractBundle::load(&root, &pins)
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    let policy = TrustedProductPolicy::load(root.join(PRODUCT_POLICY_BUNDLE_FILENAME), &pins)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;
    policy
        .validate_bundle(&bundle)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;

    // Ensure the authenticated grant view and the separately pinned policy describe the same
    // active authorization set. This prevents a stale BFF cache from widening its preflight.
    let mut local_grants = BTreeSet::new();
    for operation in bundle.operations() {
        if policy.has_grant(operation.owner_id(), operation.operation_id()) {
            local_grants.insert((
                operation.owner_id().to_owned(),
                operation.operation_id().to_owned(),
            ));
        }
    }
    if local_grants != authority_grants {
        return Err(ProductCatalogError::TrustedPolicy);
    }

    Ok(ProductOperationCatalog::from_verified_bundle(
        bundle, policy,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleManifestProjection {
    format_version: u32,
    wire_api_version: String,
    owners: Vec<BundleOwnerProjection>,
    files: Vec<BundleFileProjection>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleOwnerProjection {
    owner_id: String,
    repository: String,
    source_sha: String,
    catalog_path: String,
    catalog_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleFileProjection {
    path: String,
    sha256: String,
}

fn validate_owner_projection(
    root: &Path,
    view: &CatalogSnapshotResponse,
) -> Result<(), ProductCatalogError> {
    let manifest_path = root.join(CONTRACT_BUNDLE_MANIFEST_FILENAME);
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_BUNDLE_MANIFEST_BYTES
    {
        return Err(ProductCatalogError::ContractBundle);
    }
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|_| ProductCatalogError::ContractBundle)?;
    let manifest: BundleManifestProjection =
        serde_json::from_slice(&manifest_bytes).map_err(|_| ProductCatalogError::ContractBundle)?;
    if manifest.format_version != 2
        || manifest.wire_api_version != view.wire_api_version
        || manifest.owners.len() != view.owners.len()
        || manifest
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != manifest.files.len()
    {
        return Err(ProductCatalogError::ContractBundle);
    }
    let authority_owners = view
        .owners
        .iter()
        .map(|owner| (owner.owner_id.as_str(), owner))
        .collect::<BTreeMap<_, _>>();
    let file_digests = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for owner in &manifest.owners {
        let Some(authority_owner) = authority_owners.get(owner.owner_id.as_str()) else {
            return Err(ProductCatalogError::ContractBundle);
        };
        if owner.source_sha != authority_owner.source_commit
            || owner.catalog_sha256 != authority_owner.catalog_digest_sha256
            || !is_sha256_hex(&owner.catalog_sha256)
            || owner.catalog_path
                != format!("{}/contracts/product/v2/catalog.json", owner.repository)
            || file_digests.get(owner.catalog_path.as_str()) != Some(&owner.catalog_sha256.as_str())
            || !seen.insert(owner.owner_id.as_str())
        {
            return Err(ProductCatalogError::ContractBundle);
        }
    }
    if seen.len() != authority_owners.len() {
        return Err(ProductCatalogError::ContractBundle);
    }
    Ok(())
}

/// Load the current Authority view through a caller-supplied authenticated RPC operation.
///
/// The RPC adapter owns transport credentials and caller/workspace binding; this helper keeps
/// disk loading and integrity checks scoped to the returned immutable view.
pub fn load_product_operation_catalog_from_environment(
    view: &CatalogSnapshotResponse,
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    let root = std::env::var_os(PRODUCT_BUNDLE_VERSIONS_ROOT_ENV)
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ProductCatalogError::ContractBundle)?;
    load_product_operation_catalog_from_authority_snapshot(root, view)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_sha256_prefixed(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256_hex)
}

fn is_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
