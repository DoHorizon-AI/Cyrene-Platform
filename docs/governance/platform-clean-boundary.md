# Platform clean boundary

Status: **Normative**
Baseline: the canonical merge commit produced by this remediation

Platform owns generic Kernel semantics, Node and Runtime supervision,
Lease/Fence enforcement, Artifact identity/transfer, workspace execution, and
the control-plane portion of Plugin installation, compatibility, activation,
health, permissions, and endpoint discovery.

Platform never owns capability payload schemas, Product lifecycle/state,
Product manifests, ecosystem catalogs, protocol adapters, deployment templates,
compatibility source snapshots, or business examples. It may return an opaque
`connection_ref`; it may not proxy, parse, route, persist, or transform the
business request or response carried by that endpoint.

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

Platform 拥有通用 Kernel 语义、Node 与 Runtime 监管、Lease/Fence 强制执行、Artifact 身份/传输、workspace 执行，以及 Plugin 安装、兼容性、激活、健康状态、权限和端点发现的控制面部分。

Platform 从不拥有 capability 负载 schema、Product 生命周期/状态、Product manifest、生态目录、协议 adapter、部署模板、兼容性源码快照或业务示例。它可以返回不透明的 `connection_ref`，但不得代理、解析、路由、持久化或转换该端点承载的业务 request 或 response。

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
