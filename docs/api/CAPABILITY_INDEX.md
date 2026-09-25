# Cyrene-Platform Contract Index

This index lists contracts physically owned by Cyrene-Platform. It is not a
catalog of Product or Plugin payloads and does not describe implementation
readiness.

| Surface | Platform source | Boundary |
| --- | --- | --- |
| Kernel semantic authority | `contracts/proto/cyrene/semantic/v1/kernel_contract.proto`, `docs/contracts/kernel-semantic-contract-v1.md` | Generic identity, lifecycle, Lease/Fence, endpoint, and event semantics |
| Kernel and Node control | `contracts/proto/cyrene/core/v1/`, `contracts/proto/cyrene/core/v2/` | Generic runtime, resource, process supervision, and node control |
| Execution and Artifact Fabric | `contracts/proto/cyrene/core/v1/node_control.proto`, `contracts/schemas/artifact_transfer.schema.json` | Placement, attachment, transfer, and failure facts |
| Artifact identity | `contracts/schemas/manifests/artifact_ref.schema.json`, `contracts/schemas/manifests/portable_directory_manifest.schema.json` | Immutable content identity with an opaque producer-owned category |
| Plugin resolution | `contracts/schemas/plugin-set.schema.json`, `contracts/rust/cy-manifest` | Generic capability identity, compatibility selection, and lock evidence |
| Package lifecycle | `contracts/proto/cyrene/core/v1/plugin_lifecycle.proto`, `contracts/schemas/verified-installation-record.schema.json` | Install, activate, supervise, health, and opaque `connection_ref` |

A capability contract, Product record, Plugin manifest, component catalog, or
business example belongs to its implementation repository. Adding one does not
require a Platform source change.
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene-Platform 契约索引

本索引列出由 Cyrene-Platform 实际拥有的契约，不是 Product 或 Plugin 负载目录，也不代表这些契约已具备可投入实现的条件。

| 接口面 | Platform 源码 | 边界 |
|---|---|---|
| Kernel semantic authority | `contracts/proto/cyrene/semantic/v1/kernel_contract.proto`、`docs/contracts/kernel-semantic-contract-v1.md` | 通用身份、生命周期、Lease/Fence、endpoint 和 event 语义 |
| Kernel 与 Node control | `contracts/proto/cyrene/core/v1/`、`contracts/proto/cyrene/core/v2/` | 通用 runtime、资源、进程监管和节点控制 |
| Execution 与 Artifact Fabric | `contracts/proto/cyrene/core/v1/node_control.proto`、`contracts/schemas/artifact_transfer.schema.json` | 放置、附加、传输和故障事实 |
| Artifact identity | `contracts/schemas/manifests/artifact_ref.schema.json`、`contracts/schemas/manifests/portable_directory_manifest.schema.json` | 不可变内容身份，类别保持为由 producer 拥有的不透明值 |
| Plugin resolution | `contracts/schemas/plugin-set.schema.json`、`contracts/rust/cy-manifest` | 通用 capability 身份、兼容性选择和 lock 证据 |
| Package lifecycle | `contracts/proto/cyrene/core/v1/plugin_lifecycle.proto`、`contracts/schemas/verified-installation-record.schema.json` | 安装、激活、监管、健康状态和不透明 `connection_ref` |

Capability 契约、Product 记录、Plugin 清单、组件目录和业务示例属于相应实现仓库。增加这些内容不要求修改 Platform 源码。
