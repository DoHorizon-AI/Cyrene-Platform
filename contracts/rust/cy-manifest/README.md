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

## Portable directory manifest | Portable 目录 Manifest

`PortableDirectoryManifest` is the provider-neutral version 2 directory
index. Its canonical JCS bytes contain only `version`, path-sorted `files`,
and the logical sum `size_bytes`; `ArtifactRef.manifest_digest` points to the
resulting digest. Each file digest remains the SHA-256 of its exact raw CAS
bytes. The manifest byte length and Product metadata are separate values.
V2 is Linux-first: ASCII case-insensitive aliases are rejected at every
directory prefix, while Unicode case mapping and normalization are outside
the identity and collision key. A Windows materializer must apply its own
reserved-name, alternate-data-stream, trailing-space, and normalization rules.
Version 1 remains a legacy Python-provider format and is not re-hashed or
rewritten by this contract.

`PortableDirectoryManifest` 是 provider-neutral 的 V2 目录索引。其 canonical JCS
字节只包含 `version`、按路径排序的 `files` 和逻辑总和 `size_bytes`；
`ArtifactRef.manifest_digest` 指向该摘要。每个文件 digest 仍然是其 raw CAS 字节
的 SHA-256。manifest 自身字节长度和 Product 元数据是独立值。V2 以 Linux 为优先
目标：每个目录前缀上的 ASCII 大小写别名都会拒绝，Unicode 大小写映射和规范化不
参与身份或冲突键；Windows materializer 还须执行自己的保留名、ADS、尾部空格和
规范化限制。V1 仍是旧 Python provider 格式，本契约不会重新计算或重写它。
