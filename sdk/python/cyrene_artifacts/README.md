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
