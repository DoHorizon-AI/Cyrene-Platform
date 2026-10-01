// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/build.rs                 ║
// ║ Module: cy-workspace-web-bff build                                   ║
// ║ Role: Embed and verify the fixed Product v2 bundle compatibility pin.║
// ║                                                                    ║
// ║ 模块职责：嵌入并校验固定的 Product v2 bundle 兼容 pin。              ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const WIRE_API_VERSION: &str = "cyrene.workspace.product.v2";
const CONTRACT_API_VERSION: &str = "0.1.0";
const RELEASE_LOCK_PATH: &str =
    "tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json";
const BUILD_CONTEXT_ENV: &str = "CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_BUILD_ROOT";

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir"));
    let repository_root = manifest_dir.join("../../..");
    verify_proto_mirror(&repository_root);

    let lock_path = repository_root.join(RELEASE_LOCK_PATH);
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-env-changed={BUILD_CONTEXT_ENV}");

    let output = PathBuf::from(env::var("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("product_bundle_pins.rs");
    if !lock_path.is_file() {
        if env::var("PROFILE").as_deref() == Ok("release") {
            panic!(
                "the fixed Product v2 release lock is required for release builds: {}",
                lock_path.display()
            );
        }
        fs::write(output, render_unavailable_pins())
            .expect("write fail-closed Product v2 test build pins");
        return;
    }

    let lock_bytes = fs::read(&lock_path).expect("read fixed Product v2 release lock");
    let lock: WorkspaceProductLock =
        serde_json::from_slice(&lock_bytes).expect("parse fixed Product v2 release lock");
    validate_lock(&lock);

    if let Some(context_root) = env::var_os(BUILD_CONTEXT_ENV).map(PathBuf::from) {
        verify_build_context(&context_root, &lock);
    }

    // Native release builds embed the independent tracked pins without a BuildKit context.
    // Docker release builds provide the context and are checked above; every executable still
    // verifies the mounted bundle and policy against these pins before serving requests.
    fs::write(output, render_pins(&lock)).expect("write embedded Product v2 release pins");
}

fn verify_proto_mirror(repository_root: &Path) {
    let proto_pairs = [
        (
            repository_root.join("contracts/proto/cyrene/workspace/product/v2/product_api.proto"),
            repository_root.join(
                "contracts/rust/cy-proto/proto/cyrene/workspace/product/v2/product_api.proto",
            ),
        ),
        (
            repository_root.join("contracts/proto/cyrene/workspace/v1/workspace_fabric.proto"),
            repository_root
                .join("contracts/rust/cy-proto/proto/cyrene/workspace/v1/workspace_fabric.proto"),
        ),
    ];
    for (canonical_path, mirror_path) in proto_pairs {
        println!("cargo:rerun-if-changed={}", canonical_path.display());
        println!("cargo:rerun-if-changed={}", mirror_path.display());
        let canonical = fs::read(&canonical_path).unwrap_or_else(|error| {
            panic!(
                "canonical Workspace Product API v2 proto is required at {}: {error}",
                canonical_path.display()
            )
        });
        let mirror = fs::read(&mirror_path).unwrap_or_else(|error| {
            panic!(
                "cy-proto Workspace Product API mirror is required at {}: {error}",
                mirror_path.display()
            )
        });
        assert_eq!(
            canonical, mirror,
            "canonical Workspace proto and cy-proto mirror differ"
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceProductLock {
    format_version: u32,
    release_id: String,
    wire_api_version: String,
    contract_api_version: String,
    bundle: BundlePin,
    policy: PolicyPin,
    owners: Vec<OwnerPin>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BundlePin {
    manifest_path: String,
    manifest_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PolicyPin {
    source_path: String,
    bundle_path: String,
    schema_version: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OwnerPin {
    owner_id: String,
    repository: String,
    source_sha: String,
    catalog_path: String,
    catalog_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BundleManifest {
    format_version: u32,
    wire_api_version: String,
    owners: Vec<OwnerPin>,
    files: Vec<BundleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    path: String,
    sha256: String,
}

fn validate_lock(lock: &WorkspaceProductLock) {
    assert_eq!(
        lock.format_version, 2,
        "unsupported Product bundle lock version"
    );
    assert!(
        !lock.release_id.trim().is_empty(),
        "Product release ID is empty"
    );
    assert_eq!(
        lock.wire_api_version, WIRE_API_VERSION,
        "mixed Product wire API version"
    );
    assert_eq!(
        lock.contract_api_version, CONTRACT_API_VERSION,
        "mixed Product contract library version"
    );
    assert_eq!(lock.bundle.manifest_path, "product-contract-bundle.json");
    assert_digest(&lock.bundle.manifest_sha256, "bundle manifest SHA-256");
    assert_eq!(
        lock.policy.source_path,
        "contracts/policies/workspace-product-policy-v2.json"
    );
    assert_eq!(lock.policy.bundle_path, "workspace-product-policy-v2.json");
    assert_eq!(
        lock.policy.schema_version,
        "cyrene.workspace.product.authorization-policy.v2"
    );
    assert_digest(&lock.policy.sha256, "Product policy SHA-256");
    assert!(!lock.owners.is_empty(), "Product bundle lock has no owners");
    let mut previous_owner: Option<&str> = None;
    let mut owner_ids = BTreeSet::new();
    for owner in &lock.owners {
        if let Some(previous) = previous_owner {
            assert!(
                previous < owner.owner_id.as_str(),
                "Product owner pins are not sorted"
            );
        }
        previous_owner = Some(&owner.owner_id);
        assert!(
            owner_ids.insert(owner.owner_id.as_str()),
            "duplicate Product owner pin"
        );
        assert_valid_owner_id(&owner.owner_id);
        assert!(
            !owner.repository.is_empty(),
            "Product owner repository is empty"
        );
        assert_lower_hex(&owner.source_sha, 40, "Product owner source SHA");
        assert!(
            !owner.catalog_path.is_empty(),
            "Product owner catalog path is empty"
        );
        assert_digest(&owner.catalog_sha256, "Product owner catalog SHA-256");
    }
}

fn verify_build_context(root: &Path, lock: &WorkspaceProductLock) {
    let manifest_path = root.join(&lock.bundle.manifest_path);
    let manifest_bytes = fs::read(&manifest_path).unwrap_or_else(|error| {
        panic!(
            "pinned Product bundle manifest is unavailable at {}: {error}",
            manifest_path.display()
        )
    });
    assert_eq!(
        sha256_hex(&manifest_bytes),
        lock.bundle.manifest_sha256,
        "BuildKit Product bundle manifest does not match the compatibility lock"
    );
    let manifest: BundleManifest =
        serde_json::from_slice(&manifest_bytes).expect("parse Product bundle manifest");
    assert_eq!(
        manifest.format_version, 2,
        "unsupported Product bundle manifest version"
    );
    assert_eq!(
        manifest.wire_api_version, WIRE_API_VERSION,
        "mixed Product bundle wire API"
    );
    assert_eq!(
        manifest.owners.len(),
        lock.owners.len(),
        "Product owner pin set differs"
    );
    for (actual, expected) in manifest.owners.iter().zip(&lock.owners) {
        assert_owner_matches(actual, expected);
    }

    let files = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        files.len(),
        manifest.files.len(),
        "duplicate Product bundle file path"
    );
    for owner in &lock.owners {
        assert_eq!(
            files.get(owner.catalog_path.as_str()),
            Some(&owner.catalog_sha256.as_str()),
            "owner catalog digest differs from the Product compatibility lock"
        );
    }

    let policy_path = root.join(&lock.policy.bundle_path);
    let policy_bytes = fs::read(&policy_path).unwrap_or_else(|error| {
        panic!(
            "pinned Product policy is unavailable at {}: {error}",
            policy_path.display()
        )
    });
    assert_eq!(
        sha256_hex(&policy_bytes),
        lock.policy.sha256,
        "BuildKit Product policy does not match the compatibility lock"
    );
}

fn assert_owner_matches(actual: &OwnerPin, expected: &OwnerPin) {
    assert_eq!(
        actual.owner_id, expected.owner_id,
        "Product owner set differs from lock"
    );
    assert_eq!(
        actual.repository, expected.repository,
        "Product owner repository differs from lock"
    );
    assert_eq!(
        actual.source_sha, expected.source_sha,
        "Product owner source SHA differs from lock"
    );
    assert_eq!(
        actual.catalog_path, expected.catalog_path,
        "Product owner catalog path differs from lock"
    );
    assert_eq!(
        actual.catalog_sha256, expected.catalog_sha256,
        "Product owner catalog digest differs from lock"
    );
}

fn render_pins(lock: &WorkspaceProductLock) -> String {
    let owners = lock
        .owners
        .iter()
        .map(|owner| format!("({:?}, {:?})", owner.owner_id, owner.source_sha))
        .collect::<Vec<_>>()
        .join(",\n    ");
    format!(
        "const PRODUCT_BUNDLE_PINS_AVAILABLE: bool = true;\n\
         const PRODUCT_BUNDLE_WIRE_API_VERSION: &str = {:?};\n\
         const PRODUCT_BUNDLE_MANIFEST_SHA256: &str = {:?};\n\
         const PRODUCT_BUNDLE_POLICY_SCHEMA_VERSION: &str = {:?};\n\
         const PRODUCT_BUNDLE_POLICY_SHA256: &str = {:?};\n\
         const PRODUCT_BUNDLE_OWNER_SOURCE_SHAS: &[(&str, &str)] = &[\n    {}\n];\n",
        lock.wire_api_version,
        lock.bundle.manifest_sha256,
        lock.policy.schema_version,
        lock.policy.sha256,
        owners,
    )
}

fn render_unavailable_pins() -> String {
    "const PRODUCT_BUNDLE_PINS_AVAILABLE: bool = false;\n\
     const PRODUCT_BUNDLE_WIRE_API_VERSION: &str = \"\";\n\
     const PRODUCT_BUNDLE_MANIFEST_SHA256: &str = \"\";\n\
     const PRODUCT_BUNDLE_POLICY_SCHEMA_VERSION: &str = \"\";\n\
     const PRODUCT_BUNDLE_POLICY_SHA256: &str = \"\";\n\
     const PRODUCT_BUNDLE_OWNER_SOURCE_SHAS: &[(&str, &str)] = &[];\n"
        .to_owned()
}

fn assert_valid_owner_id(value: &str) {
    let mut chars = value.chars();
    assert!(
        chars
            .next()
            .is_some_and(|character| character.is_ascii_lowercase()),
        "Product owner ID must start with a lowercase letter"
    );
    assert!(
        chars.all(|character| character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'),
        "Product owner ID contains invalid characters"
    );
}

fn assert_digest(value: &str, label: &str) {
    assert_lower_hex(value, 64, label);
}

fn assert_lower_hex(value: &str, expected_len: usize, label: &str) {
    assert_eq!(value.len(), expected_len, "{label} has an invalid length");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} is not lowercase hexadecimal"
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
