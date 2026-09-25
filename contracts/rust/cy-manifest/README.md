# cy-manifest

Rust projections for Platform-owned Artifact identity, portable directory
manifests, and generic Plugin control-plane resolution records.

Product model, training, serving, checkpoint, validation, and lineage records
are intentionally absent. Capability payload schemas and repository Plugin
manifests live with their Plugin implementation.

## Portable directory identity

```bash
cargo run -p cy-manifest --bin cy-manifest -- hash --type portable-directory \
  contracts/schemas/examples/portable_directory_manifest.example.json
```

The V2 canonical JCS preimage contains only `version`, strictly path-sorted
`files`, and their logical `size_bytes`. Provider locations and Product metadata
are outside the identity.

## Validation

```bash
cargo test -p cy-manifest
```

本 crate 只保留 Platform 拥有的通用 Artifact 身份、portable directory 清单和
Plugin 控制面解析模型。模型、训练、部署、检查点、验证及血缘业务记录由对应
Product 或 Plugin 仓库维护。
---

<!-- Chinese Translation / 中文翻译 -->

# cy-manifest

此 crate 提供 Platform 所有的 Artifact 身份、portable directory manifest 和通用 Plugin control-plane resolution record 的 Rust 投影。

此处有意不包含 Product model、training、serving、checkpoint、validation 和 lineage record。Capability payload schema 和仓库 Plugin manifest 位于各自 Plugin 实现仓库。

## Portable Directory Identity

```bash
cargo run -p cy-manifest --bin cy-manifest -- hash --type portable-directory \
  contracts/schemas/examples/portable_directory_manifest.example.json
```

V2 canonical JCS 原像只包含 `version`、按路径严格排序的 `files` 和其逻辑 `size_bytes`。Provider location 与 Product metadata 不属于该身份。

## 验证

```bash
cargo test -p cy-manifest
```
