// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/build.rs                ║
// ║ Module: CYRENE Platform                                             ║
// ║ Role: Generate the BFF projection view from the canonical TCK.      ║
// ║                                                                    ║
// ║ 模块：CYRENE Platform                                               ║
// ║ 职责：从规范 TCK 生成 BFF projection 视图。                           ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::env;
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

const OWNER_NAMES: &[&str] = &[
    "CATALYST",
    "YIELD",
    "REACTOR",
    "EXCHANGE",
    "ECHO",
    "NAVIGATOR",
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir"));
    let repository_root = manifest_dir.join("../../..");
    let tck_path = repository_root
        .join("contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv");
    let canonical_proto =
        repository_root.join("contracts/proto/cyrene/workspace/v1/workspace_fabric.proto");
    let generated_proto = repository_root
        .join("contracts/rust/cy-proto/proto/cyrene/workspace/v1/workspace_fabric.proto");
    println!("cargo:rerun-if-changed={}", tck_path.display());
    println!("cargo:rerun-if-changed={}", canonical_proto.display());
    println!("cargo:rerun-if-changed={}", generated_proto.display());

    let source = fs::read_to_string(&tck_path).unwrap_or_else(|error| {
        panic!(
            "canonical Product projection TCK is required at {}: {error}",
            tck_path.display()
        )
    });
    let tck_bytes = fs::read(&tck_path).expect("read canonical Product projection TCK bytes");
    let tck_sha256 = Sha256::digest(tck_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let entries = parse_manifest(&source);
    let canonical_enum = read_operation_enum(&canonical_proto);
    let generated_enum = read_operation_enum(&generated_proto);
    let manifest_operations = entries
        .iter()
        .map(|entry| entry.wire_operation.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        canonical_enum, generated_enum,
        "the Workspace proto source and cy-proto mirror differ"
    );
    assert_eq!(
        manifest_operations, canonical_enum,
        "Product projection TCK keys must exactly match the WorkspaceProductApiOperation enum"
    );
    let generated = render_manifest(&entries, &tck_sha256);
    let output = PathBuf::from(env::var("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("product_projection_manifest.rs");
    fs::write(output, generated).expect("write generated Product projection view");
}

fn read_operation_enum(path: &std::path::Path) -> Vec<String> {
    let source = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "Workspace Product API proto is required at {}: {error}",
            path.display()
        )
    });
    let enum_body = source
        .split("enum WorkspaceProductApiOperation {")
        .nth(1)
        .and_then(|remainder| remainder.split_once('}').map(|(body, _)| body))
        .unwrap_or_else(|| {
            panic!(
                "WorkspaceProductApiOperation enum missing in {}",
                path.display()
            )
        });
    enum_body
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                return None;
            }
            let name = line.split('=').next()?.trim();
            (!name.is_empty() && name != "WORKSPACE_PRODUCT_API_OPERATION_UNSPECIFIED")
                .then(|| name.to_owned())
        })
        .collect()
}

#[derive(Debug)]
struct Entry {
    owner: String,
    wire_operation: String,
    product_operation_id: String,
    kind: String,
    product_contract: String,
}

fn parse_manifest(source: &str) -> Vec<Entry> {
    let mut lines = source.lines();
    let header = lines.next().unwrap_or_default();
    assert_eq!(
        header, "owner\twire_operation\tproduct_operation_id\tkind\tproduct_contract",
        "Product projection manifest header changed"
    );

    let mut entries = Vec::new();
    let mut previous_operations = std::collections::BTreeSet::new();
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            5,
            "invalid Product projection row {}",
            index + 2
        );
        let (owner, wire_operation, product_operation_id, kind, product_contract) =
            (fields[0], fields[1], fields[2], fields[3], fields[4]);
        let expected_operation =
            format!("WORKSPACE_PRODUCT_API_OPERATION_{:02}", entries.len() + 1);
        assert_eq!(
            wire_operation, expected_operation,
            "operation keys must be contiguous and in canonical order"
        );
        assert!(
            OWNER_NAMES.contains(&owner),
            "unknown canonical Product owner {owner}"
        );
        assert!(
            matches!(kind, "READ" | "COMMAND"),
            "unsupported Product operation kind {kind}"
        );
        assert!(
            !product_operation_id.trim().is_empty(),
            "empty Product operationId"
        );
        assert!(
            product_contract.starts_with("Cyrene-"),
            "invalid Product contract path"
        );
        assert!(
            previous_operations.insert(wire_operation.to_owned()),
            "duplicate Product operation key {wire_operation}"
        );
        entries.push(Entry {
            owner: owner.to_owned(),
            wire_operation: wire_operation.to_owned(),
            product_operation_id: product_operation_id.to_owned(),
            kind: kind.to_owned(),
            product_contract: product_contract.to_owned(),
        });
    }
    assert_eq!(
        entries.len(),
        13,
        "canonical Product projection must have 13 operations"
    );
    entries
}

fn render_manifest(entries: &[Entry], tck_sha256: &str) -> String {
    let mut output = format!(
        "const GENERATED_PRODUCT_PROJECTION_TCK_SHA256: &str = {tck_sha256:?};\nconst GENERATED_PRODUCT_PROJECTION_ROWS: &[GeneratedProjectionRow] = &[\n"
    );
    for entry in entries {
        output.push_str(&format!(
            "    GeneratedProjectionRow {{ owner: {:?}, wire_operation: {:?}, product_operation_id: {:?}, kind: {:?}, product_contract: {:?} }},\n",
            entry.owner,
            entry.wire_operation,
            entry.product_operation_id,
            entry.kind,
            entry.product_contract,
        ));
    }
    output.push_str("];\n");
    output
}
