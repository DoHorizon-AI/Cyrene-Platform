# How to Read Cyrene-Platform

Trace a Platform mechanism from its contract to its implementations and tests.

1. Start with `contracts/proto/` or `contracts/schemas/` to identify the
   versioned semantic boundary.
2. Read `kernel/crates/cy-kernel-api/` for Kernel types and
   `kernel/crates/cy-kernel-daemon/` for node-local lifecycle behavior.
3. Read `framework/crates/` for generic execution control, package lifecycle,
   plugin resolution, and fabric mechanisms.
4. Read `sdk/` for language projections of those contracts.
5. Finish with the nearest TCK and repository boundary guard under
   `contracts/tck/` or `tooling/ci/`.

Ask which owner controls the state being changed. Node-local leases, fencing,
process supervision, transport, and immutable Artifact references are Platform
mechanisms. User intent, lifecycle policy, billing, routing decisions, concrete
engines, and vendor adapters belong to consumers or plugins.

Cross-repository workflow traces and current repository locations belong in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
---

<!-- Chinese Translation / 中文翻译 -->

# 如何阅读 Cyrene-Platform

沿着契约、实现和测试追踪 Platform 机制。

1. 从 `contracts/proto/` 或 `contracts/schemas/` 入手，确定版本化语义边界。
2. 阅读 `kernel/crates/cy-kernel-api/` 了解 Kernel 类型，并阅读 `kernel/crates/cy-kernel-daemon/` 了解节点本地生命周期行为。
3. 阅读 `framework/crates/`，了解通用执行控制、package 生命周期、plugin 解析和 fabric 机制。
4. 阅读 `sdk/`，了解这些契约的语言投影。
5. 最后查看最近的 TCK 和仓库边界检查器，位置在 `contracts/tck/` 或 `tooling/ci/`。

明确此次变更的状态由哪个所有者控制。节点本地 Lease、fencing、进程监管、传输和不可变 Artifact 引用是 Platform 机制。用户意图、生命周期策略、计费、路由决策、具体引擎和厂商 adapter 属于 consumer 或 plugin。

跨仓工作流追踪和当前仓库位置请参阅 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace)。
