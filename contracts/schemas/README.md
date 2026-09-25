# Platform JSON Schemas | Platform JSON Schema

These schemas describe only stable Platform control-plane and Artifact Plane
contracts. Capability payload schemas, Product manifests, component catalogs,
and Plugin repository manifests are owned by their implementation repository.

这些 Schema 仅描述稳定的 Platform 控制面与 Artifact Plane 契约。能力载荷、
Product 清单、组件目录和 Plugin 仓库清单由其实现仓库拥有。

| Schema | Authority |
| --- | --- |
| `artifact_transfer.schema.json` | Provider-neutral transfer records around `ArtifactRef`. |
| `manifests/artifact_ref.schema.json` | Immutable content reference with an opaque producer-owned category. |
| `manifests/portable_directory_manifest.schema.json` | Cross-language portable directory index. |
| `plugin-set.schema.json` | Generic capability requirements and deterministic resolution locks. |
| `verified-installation-record.schema.json` | Digest-bound installer evidence consumed by Platform. |
---

<!-- Chinese Translation / 中文翻译 -->

# Platform JSON Schema

这些 schema 只描述稳定的 Platform control-plane 和 Artifact Plane 契约。Capability payload schema、Product manifest、component catalog 和 Plugin repository manifest 由其实现仓库拥有。

| Schema | 权威内容 |
|---|---|
| `artifact_transfer.schema.json` | 围绕 `ArtifactRef` 的 Provider-neutral transfer record。 |
| `manifests/artifact_ref.schema.json` | 不可变内容引用；category 是 producer 拥有的不透明值。 |
| `manifests/portable_directory_manifest.schema.json` | 跨语言 portable directory index。 |
| `plugin-set.schema.json` | 通用 capability requirement 和确定性 resolution lock。 |
| `verified-installation-record.schema.json` | Platform 使用的、与 digest 绑定的 installer 证据。 |
