# cyrene-artifacts

Provider-neutral Python bindings for `ArtifactRef`, portable directory
publication, verified resolution, and staging.

`ArtifactKind` is an opaque producer-owned string. Adding a Product category
requires no Platform release. Product lineage and lifecycle metadata are kept
outside this SDK.

`publish_portable_directory()` writes the public V2 directory contract. Its JCS
identity contains only sorted logical POSIX paths, raw blob digests, and the
logical byte total. `publish()` keeps the provider-private V1 directory format
readable for existing local stores; it is not a public Product contract.

本 SDK 只提供与供应商无关的 `ArtifactRef`、portable directory 发布、校验解析和
staging。`ArtifactKind` 是生产者拥有的不透明字符串；新增 Product 分类无需修改
Platform。Product 血缘与生命周期元数据不在本 SDK 中。
---

<!-- Chinese Translation / 中文翻译 -->

# cyrene-artifacts

提供与 Provider 无关的 Python binding，用于 `ArtifactRef`、portable directory 发布、已验证解析和 staging。

`ArtifactKind` 是由 producer 所有的不透明字符串。增加 Product category 不需要发布 Platform 新版本。Product lineage 和 lifecycle metadata 保留在此 SDK 之外。

`publish_portable_directory()` 写入公开 V2 directory contract。其 JCS identity 只包含排序后的逻辑 POSIX 路径、原始 blob digest 和逻辑字节总数。`publish()` 为现有本地 store 保留可读的 Provider-private V1 directory format；它不是公开 Product contract。
