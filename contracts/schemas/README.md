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
