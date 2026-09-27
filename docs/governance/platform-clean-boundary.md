# Platform clean boundary

Status: **Normative**
Baseline: the canonical merge commit produced by this remediation

Platform owns generic Kernel semantics, Node and Runtime supervision,
Lease/Fence enforcement, Artifact identity/transfer, Workspace execution, and
the generic Relay control plane. Generic Relay exposes an opaque
`connection_ref` and does not inspect Product payloads. Directory-backed
identity, membership, caller scope, and authorization also remain Platform
authorities.

## Bounded Workspace application hosts

The current Platform source retains the Workspace Connector and Workspace Web
BFF application hosts. This exception covers the existing Workspace caller,
control-plane, authorization and operation-projection modules; the typed
owner-specific HTTP adapters; and the BFF's fixed routes and read-only,
provenance-pinned Product contract loader.

The Connector maps only the closed Workspace Product operation enum to fixed
owner routes. Caller-provided resource identifiers use validated path segments.
The BFF exposes the existing identity, Directory, device-approval, health and
single Product-operation routes. These hosts do not own Product-private state,
schemas, workflows, retries, persistence or service decisions, and they do not
accept arbitrary Product URLs or open-ended business routes.

Cyrene-Client owns the Workspace Azure Container Apps and Bicep deployment
templates (transferred by [Client merge `07a8516`](https://github.com/DoHorizon-AI/Cyrene-Client/commit/07a851624193f22c524b14c81f382e770fd63e0e)).
Platform retains the current Rust host source and its build/package path while
Client has no runnable Rust host for these components. Source
ownership may move after Client has a buildable host with pinned Platform
contracts, a source CI and immutable image-artifact chain, and a deployment
path that consumes that verified artifact. This ownership record does not
assert production deployment readiness.

Platform does not own capability payload schemas, Product-private domain
contracts, Product service implementations, authoritative ecosystem catalogs,
deployment templates, compatibility source snapshots, or business examples.
Its Workspace host exception does not create ownership of the Product data or
workflows carried by those bounded operations.

## Zero-change extension rule

A Product or Plugin consumes the published Platform contracts without a
Platform source change. A proposed Platform addition must identify at least two
independent consumers or a Kernel-level invariant and must include a generic
contract test. One Product's convenience, language, protocol, deployment shape,
or vocabulary is insufficient.

Repository-owned `plugin.manifest.json` files define Plugin identity,
capabilities, methods, payload schema references, runtime language, and
protocol. Platform normalizes only the fields needed for generic compatibility
selection. For a managed service package, the package supplies a language-
neutral launch command. Platform starts that verified process, validates the
readiness identity, and returns its opaque endpoint.

## Removed compatibility surfaces

The following implemented legacy surfaces were removed after reverse-dependency
checks and owner migration:

- ten named v0 capability SPI Protobuf contracts and their Rust adapters;
- `cy-extension-registry`, `cy-local-transport`, and `BuiltinInMemoryStorage`;
- Capability Execution Service, its client SDK/TCK, stdio worker protocol, and
  worker SDK/shim;
- model-provider and message-connector payload contracts;
- Product run/environment/model-version implementations;
- AI model, hardware, training, runtime, checkpoint, validation, planning, and
  Artifact-lineage manifests;
- v0 `plugin.toml`, Product service manifests/catalogs, and the Platform copy of
  `media.processor.v1`.

`tooling/ci/check-no-legacy-surface.sh` prevents those paths, symbols, Product
identities, and package data-plane operations from returning.

## Evidence boundary

A local or Hosted build proves only the checks it actually ran. GPU, external
provider, Kubernetes, and complete Product lifecycle evidence remain separate
and must not be inferred from Platform compilation.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 清晰边界

状态：**规范性文件**
基线：本次治理修复生成的规范合并提交

Platform 拥有通用 Kernel 语义、Node 与 Runtime 监管、Lease/Fence 强制执行、Artifact 身份/传输、Workspace 执行和通用 Relay 控制面。通用 Relay 只暴露不透明的 `connection_ref`，不会检查 Product payload。Directory 身份、成员关系、caller scope 与授权也由 Platform 负责。

## 有界 Workspace 应用宿主

当前 Platform 源码仍保留 Workspace Connector 与 Workspace Web BFF 应用宿主。例外范围仅含现有 Workspace caller、控制面、授权与 operation projection 模块，类型化 owner HTTP adapter，以及 BFF 固定路由和只读、带 provenance 校验的 Product 合同 loader。

Connector 只将封闭的 Workspace Product operation enum 映射到固定 owner 路由。调用方提供的 resource ID 通过已校验的 path segment 传递。BFF 只暴露现有 identity、Directory、设备审批、health 与单一 Product operation 路由。这些宿主不拥有 Product 私有状态、schema、workflow、retry、持久化或服务决策，也不接受任意 Product URL 或开放式业务路由。

Cyrene-Client 负责 Workspace Azure Container Apps 与 Bicep 部署模板（由 [Client merge `07a8516`](https://github.com/DoHorizon-AI/Cyrene-Client/commit/07a851624193f22c524b14c81f382e770fd63e0e) 完成迁移）。Client 尚无可运行这些组件的 Rust host，因此 Platform 暂时保留现有 Rust host 源码和 build/package 路径。只有当 Client 提供带固定 Platform 契约依赖的可构建 host、source CI 与不可变 image artifact 链路，以及消费已验证 artifact 的部署路径后，才迁移 host 源码。本归属记录不表示这些服务已达到生产部署就绪状态。

Platform 不拥有 capability payload schema、Product 私有领域契约、Product 服务实现、权威生态 catalog、部署模板、兼容性源码快照或业务示例。Workspace 宿主例外不把这些有界 operation 承载的 Product 数据或 workflow 变为 Platform 所有。

## 零改动扩展规则

Product 或 Plugin 应直接消费已发布的 Platform 契约，无需修改 Platform 源码。提议新增 Platform 功能时，必须列出至少两个独立 consumer，或一项 Kernel 级不变量，并附带通用契约测试。单个 Product 的便利需求、语言、协议、部署形式或词汇不足以成为理由。

仓库自有的 `plugin.manifest.json` 定义 Plugin 身份、capability、方法、负载 schema 引用、runtime 语言和协议。Platform 只规范化通用兼容性选择所需的字段。受管 Service package 提供与语言无关的启动命令；Platform 启动已验证进程、校验 readiness 身份，并返回不透明端点。

## 已移除的兼容界面

在完成反向依赖检查和 owner 迁移后，移除了以下已实现的旧界面：

- 十个命名的 v0 capability SPI Protobuf 契约及其 Rust adapter；
- `cy-extension-registry`、`cy-local-transport` 和 `BuiltinInMemoryStorage`；
- Capability Execution Service、其 client SDK/TCK、stdio worker 协议和 worker SDK/shim；
- model-provider 和 message-connector 负载契约；
- Product run/environment/model-version 实现；
- AI model、hardware、training、runtime、checkpoint、validation、planning 和 Artifact lineage manifest；
- v0 `plugin.toml`、Product service manifest/catalog，以及 Platform 中的 `media.processor.v1` 副本。

`tooling/ci/check-no-legacy-surface.sh` 会阻止这些路径、符号、Product 身份和 package data-plane 操作重新出现。

## 证据边界

本地或 Hosted build 只证明实际运行过的检查。GPU、外部 provider、Kubernetes 和完整 Product lifecycle 的证据仍需单独取得，不能由 Platform 编译成功推断。
