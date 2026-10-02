// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/catalog_loader.rs   ║
// ║ Module: cy_workspace_web_bff::catalog_loader                       ║
// ║ Role: Load the build-pinned Product v2 catalog and policy bundle.   ║
// ║                                                                    ║
// ║ 模块职责：加载构建时固定 pin 的 Product v2 catalog 与 policy bundle。║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
};

use crate::product::{ProductCatalogError, ProductOperationCatalog};

/// Name of the versioned release bundle manifest.
///
/// 版本化 release bundle manifest 文件名。
pub const CONTRACT_BUNDLE_MANIFEST_FILENAME: &str = "product-contract-bundle.json";

/// Name of the separately pinned Platform authorization policy artifact.
///
/// 独立 pin 的 Platform authorization policy artifact 文件名。
pub const PRODUCT_POLICY_BUNDLE_FILENAME: &str = "workspace-product-policy-v2.json";

/// Environment variable naming the immutable Product v2 bundle mount.
///
/// 指向不可变 Product v2 bundle 挂载目录的环境变量名称。
pub const PRODUCT_CONTRACT_ROOT_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT_V2";

include!(concat!(env!("OUT_DIR"), "/product_bundle_pins.rs"));

/// Load and compile the pinned Product v2 catalog and trusted Platform policy.
///
/// 加载并编译固定 pin 的 Product v2 catalog 与受信 Platform policy。
pub fn load_product_operation_catalog(
    contract_root: impl AsRef<Path>,
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    let pins = expected_product_bundle_pins()?;
    let root = contract_root
        .as_ref()
        .canonicalize()
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if !root.is_dir() {
        return Err(ProductCatalogError::ContractBundle);
    }

    let bundle = ProductContractBundle::load(&root, &pins)
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    let policy = TrustedProductPolicy::load(root.join(PRODUCT_POLICY_BUNDLE_FILENAME), &pins)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;
    policy
        .validate_bundle(&bundle)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;
    Ok(ProductOperationCatalog::from_verified_bundle(
        bundle, policy,
    ))
}

/// Load the fixed-version Product v2 bundle path from process configuration.
///
/// 从进程配置读取固定版本 Product v2 bundle 路径。
pub fn load_product_operation_catalog_from_environment(
) -> Result<ProductOperationCatalog, ProductCatalogError> {
    let root = std::env::var_os(PRODUCT_CONTRACT_ROOT_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ProductCatalogError::ContractBundle)?;
    load_product_operation_catalog(root)
}

/// Load both the operation catalog and the pinned contract snapshot.
pub fn load_product_catalog_and_snapshot(
    contract_root: impl AsRef<Path>,
) -> Result<
    (
        ProductOperationCatalog,
        cy_workspace_control_plane::ContractSnapshot,
    ),
    ProductCatalogError,
> {
    let pins = expected_product_bundle_pins()?;
    let root = contract_root
        .as_ref()
        .canonicalize()
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    if !root.is_dir() {
        return Err(ProductCatalogError::ContractBundle);
    }

    let bundle = ProductContractBundle::load(&root, &pins)
        .map_err(|_| ProductCatalogError::ContractBundle)?;
    let policy = TrustedProductPolicy::load(root.join(PRODUCT_POLICY_BUNDLE_FILENAME), &pins)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;
    policy
        .validate_bundle(&bundle)
        .map_err(|_| ProductCatalogError::TrustedPolicy)?;
    let bundle_arc = std::sync::Arc::new(bundle);
    let policy_arc = std::sync::Arc::new(policy);
    let catalog = ProductOperationCatalog::from_arcs(
        std::sync::Arc::clone(&bundle_arc),
        std::sync::Arc::clone(&policy_arc),
    );
    let snapshot = cy_workspace_control_plane::ContractSnapshot {
        generation: 1,
        bundle: bundle_arc,
        policy: policy_arc,
        pins,
        activated_at_unix_ms: cy_workspace_control_plane::now_unix_ms(),
    };
    Ok((catalog, snapshot))
}

pub fn load_product_catalog_and_snapshot_from_environment() -> Result<
    (
        ProductOperationCatalog,
        cy_workspace_control_plane::ContractSnapshot,
    ),
    ProductCatalogError,
> {
    let root = std::env::var_os(PRODUCT_CONTRACT_ROOT_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ProductCatalogError::ContractBundle)?;
    load_product_catalog_and_snapshot(root)
}

fn expected_product_bundle_pins() -> Result<ProductBundlePins, ProductCatalogError> {
    if !PRODUCT_BUNDLE_PINS_AVAILABLE {
        return Err(ProductCatalogError::ReleasePinUnavailable);
    }
    let owner_source_shas = PRODUCT_BUNDLE_OWNER_SOURCE_SHAS
        .iter()
        .map(|(owner_id, source_sha)| ((*owner_id).to_owned(), (*source_sha).to_owned()))
        .collect::<BTreeMap<_, _>>();
    Ok(ProductBundlePins::new(
        PRODUCT_BUNDLE_WIRE_API_VERSION.to_owned(),
        PRODUCT_BUNDLE_MANIFEST_SHA256.to_owned(),
        owner_source_shas,
        PRODUCT_BUNDLE_POLICY_SCHEMA_VERSION.to_owned(),
        PRODUCT_BUNDLE_POLICY_SHA256.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BUNDLE_ROOT_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_TEST_PRODUCT_CONTRACT_ROOT_V2";

    /// Exercise runtime loading against the same separately verified bundle used for packaging.
    ///
    /// This smoke test is ignored by default because normal source checkouts do not contain the
    /// external release bundle. Run it with TEST_BUNDLE_ROOT_ENV pointing at a verified bundle.
    #[test]
    #[ignore = "requires a separately verified Product v2 release bundle"]
    fn loads_pinned_catalog_and_keeps_navigator_append_denied() {
        let root = std::env::var_os(TEST_BUNDLE_ROOT_ENV)
            .map(PathBuf::from)
            .expect("set the separately verified Product v2 bundle root");
        let catalog = load_product_operation_catalog(root)
            .expect("the immutable lock and release bundle match");

        assert!(catalog.get("catalyst", "workspaceListDatasets").is_some());
        assert!(catalog.has_grant("catalyst", "workspaceListDatasets"));
        assert!(catalog
            .get(
                "navigator",
                "append_events_api_v1_harness_workspaces__workspace_id__sessions__session_id__append_post"
            )
            .is_some());
        assert!(!catalog.has_grant(
            "navigator",
            "append_events_api_v1_harness_workspaces__workspace_id__sessions__session_id__append_post"
        ));
    }
}
