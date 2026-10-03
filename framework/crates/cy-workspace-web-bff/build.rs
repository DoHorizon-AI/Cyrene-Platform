//! Build-time mirror guard for the canonical Workspace Product API contracts.
//!
//! The Product catalog and authorization policy are runtime data owned by
//! Workspace Authority. This build script intentionally embeds no catalog,
//! source-commit, bundle, or policy digest pins.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir"));
    let repository_root = manifest_dir.join("../../..");
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
                "canonical Workspace proto is required at {}: {error}",
                canonical_path.display()
            )
        });
        let mirror = fs::read(&mirror_path).unwrap_or_else(|error| {
            panic!(
                "cy-proto Workspace mirror is required at {}: {error}",
                mirror_path.display()
            )
        });
        assert_eq!(
            canonical, mirror,
            "canonical Workspace proto and cy-proto mirror differ"
        );
    }
}
