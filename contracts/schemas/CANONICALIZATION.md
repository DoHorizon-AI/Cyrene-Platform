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
---

<!-- Chinese Translation / 中文翻译 -->

# Manifest 规范化与 Hash

当通用 control-plane record 需要确定性字节时，Platform 使用 RFC 8785 JSON Canonicalization Scheme（JCS）。Rust 参考实现位于 `contracts/rust/cy-manifest`，并通过 `serde_jcs` 处理数字和字符串序列化。

当前公开的哈希 manifest 是 `manifests/portable_directory_manifest.schema.json`。其完整原像为：

```text
{"files":[{"digest":"sha256:<raw-blob>","path":"relative/name","size_bytes":N}],"size_bytes":N,"version":2}
```

files 数组严格按照规范 POSIX 相对路径排序。每个文件 digest 标识其原始字节，`size_bytes` 是所有成员字节数的逻辑总和。`ArtifactRef.digest` 和 `manifest_digest` 标识规范索引。Provider locator、本地路径、时间戳、Product lineage 和 Product 状态不会进入该原像。

旧的 model、training、checkpoint、runtime、validation、planning 和 Artifact lineage manifest 已在 Product owner 建立各自独立契约后从 Platform 移除。若需重新引入这些 payload，应由相应所有者仓库实现，而不是扩展 `cy-manifest`。
