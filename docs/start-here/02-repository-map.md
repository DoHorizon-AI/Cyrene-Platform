# Platform Ownership Map

Cyrene-Platform owns only the generic foundation described by
[`repository-policy.yaml`](../../repository-policy.yaml): versioned contracts,
the Rust Kernel, process isolation and resource mechanisms, product-neutral
execution control, Platform daemon units, and repository-local CI.

It does not own Product lifecycle state, concrete AI engines, connector
adapters, deployment templates, compatibility snapshots, or cross-repository
catalogs. A new Product, plugin, or adapter must consume released Platform
contracts without adding its identity to this repository.

The current multi-repository map, target branches, and dependency pins are
mutable integration data owned by
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace). Consult
that repository instead of copying its catalog here.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 职责图

Cyrene-Platform 只拥有 `repository-policy.yaml` 所述的通用基础：版本化契约、Rust Kernel、进程隔离与资源机制、与 Product 无关的执行控制、Platform daemon unit，以及仓库本地 CI。

它不拥有 Product 生命周期状态、具体 AI 引擎、connector adapter、部署模板、兼容性快照或跨仓库目录。新 Product、plugin 或 adapter 必须消费已发布的 Platform 契约，而不得将其身份加入本仓库。

当前多仓库地图、目标分支和依赖 pin 是可变集成数据，由 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) 所有。请查阅该仓库，不要在此复制其目录。
