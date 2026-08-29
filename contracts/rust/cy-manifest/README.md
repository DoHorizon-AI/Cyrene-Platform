# cy-manifest

Core CYRENE manifest types (Hardware / Model / Workload / Runtime) with a
deterministic canonical hash.

The JSON Schemas under `contracts/schemas/manifests/` are the single source of truth, and
`contracts/schemas/CANONICALIZATION.md` is the normative hashing spec. This crate is the
in-repository implementation. External language bindings must produce byte-identical
`canonical_bytes` before they are accepted as compatible.

## Library

```rust
use cy_manifest::{runtime_id, RuntimeManifest, Manifest};

fn runtime_id_from_json(json: &str) -> Result<String, serde_json::Error> {
    let manifest: RuntimeManifest = serde_json::from_str(json)?;
    let id = runtime_id(&manifest); // "sha256:<hex>", runtime_id field excluded from preimage
    let _bytes = manifest.canonical_bytes();
    Ok(id)
}
```

## CLI

```bash
# Prints the computed runtime_id for a JSON or YAML RuntimeManifest.
cargo run -p cy-manifest --bin cy-manifest -- hash contracts/schemas/examples/runtime_manifest.example.json
```

## Test

```bash
cargo test -p cy-manifest   # determinism + known-answer tests
```
