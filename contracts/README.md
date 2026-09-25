# Contracts

`contracts/` is the only cross-repository dependency boundary. The normative
semantic authority is [Kernel Semantic Contract v1](../docs/contracts/kernel-semantic-contract-v1.md);
wire schemas and language libraries are projections of that authority.

- `proto/cyrene/semantic/v1/` defines the language-neutral Kernel vocabulary.
- the remaining `proto/` packages define transport-specific Platform control
  messages that import or map to that vocabulary.
- `schemas/` defines manifests and canonical resources.
- `rust/` contains Rust mirrors and generated bindings.
- `tck/kernel-semantic/v1/` contains the Python/Kotlin/Rust acceptance vectors
  for revision negotiation, validation, matching, lifecycle, authority and
  event replay. `tck/worker-control/v1/` separately covers the Kernel
  Worker control channel; clients use the paired
  `KernelAuthorityService` and restricted `WorkerControlService` projections.

Advanced services may depend on released contract artifacts or a checked-out
core repository. They must not depend on private paths inside the kernel or
framework implementations.

Compatibility Core RPCs live under the versioned `cyrene.core.v1` Proto module. The Rust
`cy-proto` crate exposes those generated types at `cyrene::core::v1` and keeps
generation in `OUT_DIR`; the checked-in descriptor and fixtures are the
cross-language compatibility baseline.

New interfaces must use the nine semantic nouns from `cyrene.semantic.v1`.
`cy-kernel-contract` is the pure Rust projection; `cy-proto` is the Protobuf
projection. Neither projection may add hidden semantics of its own.

The frozen semantic model does not imply that every compatibility transport is
already conforming. `cyrene.core.v1`, Node Agent and Hardware Adapter messages
remain migration projections until they pass the Kernel semantic TCK and the
conformance conditions listed in the normative document.

The former version-0 package and agent service were removed in the P1 cutover.
Runtime source and contract source must not reintroduce those names. The
former local stdio data-plane contract was removed. Plugin-owned endpoint
protocols remain outside Platform and are not part of Core RPC.
---

<!-- Chinese Translation / 中文翻译 -->

# 契约

`contracts/` 是唯一的跨仓依赖边界。规范语义权威是 [Kernel Semantic Contract v1](../docs/contracts/kernel-semantic-contract-v1.md)；线协议 schema 和语言库都是它的投影。

- `proto/cyrene/semantic/v1/` 定义与语言无关的 Kernel 词汇。
- 其余 `proto/` package 定义特定传输方式的 Platform control message，这些消息会导入或映射到上述词汇。
- `schemas/` 定义 manifest 和规范资源。
- `rust/` 包含 Rust 镜像和生成的绑定。
- `tck/kernel-semantic/v1/` 包含 Python/Kotlin/Rust 验收向量，用于验证 revision 协商、校验、匹配、生命周期、authority 和事件 replay。`tck/worker-control/v1/` 单独覆盖 Kernel Worker control channel；客户端使用配套的 `KernelAuthorityService` 和受限的 `WorkerControlService` 投影。

高级 Service 可以依赖已发布的契约制品或已检出的 Core 仓库，但不得依赖 Kernel 或 Framework 实现中的私有路径。

兼容性 Core RPC 位于版本化的 `cyrene.core.v1` Proto module 下。Rust `cy-proto` crate 将生成类型导出为 `cyrene::core::v1`，并将生成文件放在 `OUT_DIR`；已检入的 descriptor 和 fixture 构成跨语言兼容性基线。

新 interface 必须使用 `cyrene.semantic.v1` 中的九个语义名词。`cy-kernel-contract` 是纯 Rust 投影，`cy-proto` 是 Protobuf 投影。两种投影都不得自行添加隐藏语义。

语义模型被冻结，不代表所有兼容性传输已经符合要求。通过 Kernel semantic TCK 及规范文档列出的符合性条件前，`cyrene.core.v1`、Node Agent 和 Hardware Adapter 消息仍然只是迁移投影。

P1 cutover 已移除原 version-0 package 和 agent service。Runtime source 和 contract source 不得重新引入这些名称。旧的本地 stdio data-plane 契约也已移除。Plugin 所有的 endpoint 协议留在 Platform 之外，不属于 Core RPC。
