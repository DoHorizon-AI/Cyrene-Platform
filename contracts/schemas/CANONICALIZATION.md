# Manifest Canonicalization & Hashing

Platform uses RFC 8785 JSON Canonicalization Scheme (JCS) when a generic
control-plane record needs deterministic bytes. The Rust reference is
`contracts/rust/cy-manifest` and delegates number/string serialization to
`serde_jcs`.

The current public hashed manifest is
`manifests/portable_directory_manifest.schema.json`. Its complete preimage is:

```text
{"files":[{"digest":"sha256:<raw-blob>","path":"relative/name","size_bytes":N}],"size_bytes":N,"version":2}
```

The files array is strictly sorted by canonical POSIX-relative path. Every file
digest identifies its raw bytes, and `size_bytes` is the logical sum of member
bytes. `ArtifactRef.digest` and `manifest_digest` identify the canonical index.
Provider locators, local paths, timestamps, Product lineage, and Product state
never enter this preimage.

Platform 对需要确定性字节的通用控制面记录使用 RFC 8785 JCS。当前公开的哈希清单只有
`portable_directory_manifest.schema.json`；其哈希原像仅包含版本、严格排序的逻辑路径、
原始文件摘要与逻辑字节总和。供应商地址、本地路径、时间戳、Product 血缘与业务状态均不进入原像。

The legacy model, training, checkpoint, runtime, validation, planning, and
Artifact lineage manifests were removed from Platform after their Product
owners introduced independent contracts. Reintroducing those payloads requires
a separate owning repository rather than an extension to `cy-manifest`.
