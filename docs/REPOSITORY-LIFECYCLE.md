# Repository Lifecycle: Cyrene-Platform / 仓库生命周期

Cyrene-Platform is a `PUBLIC_FOUNDATION` with public visibility. It is an
independently buildable trust-base component rather than a standalone Product.

Cyrene-Platform 是公开可见的 `PUBLIC_FOUNDATION`，属于可独立构建的信任基础组件，
而不是独立 Product。

## Ownership / 归属

The repository owns foundational Protobuf and schema contracts, the Rust
Kernel, process isolation and resource mechanisms, Product-neutral execution
control, Platform daemon units, and repository-local CI. It does not own Product
lifecycle state, concrete AI engines, Product infrastructure, compatibility
snapshots, or cross-repository catalogs.

本仓库拥有基础 Protobuf/Schema 契约、Rust Kernel、进程隔离与资源机制、与 Product
无关的执行控制、Platform daemon 以及仓库内 CI；不拥有 Product 生命周期状态、具体
AI 引擎、Product 基础设施、兼容性快照或跨仓目录。

## Build, branches, and release / 构建、分支与发布

- Independent build / 独立构建：`true`
- Default and integration branch / 默认与集成分支：`develop` (unprotected)
- Main promotion / 主线提升：green `develop` -> protected `main`
- Release promotion / 发布提升：buildable green `main` -> protected `release`
- Release role / 发布角色：`COMPONENT_RELEASE`
- Versioning / 版本：repository-scoped SemVer with immutable `v{version}` tags
- Hosted CI authority / CI 权威：`github`
- Deployment authority / 部署权威：`consuming_repository`
- Multi-repository build required / 是否要求多仓构建：`false`

Azure Pipelines consumes only immutable artifacts produced by successful
GitHub Actions runs and performs CD without source checkout, build, or test.

Azure Pipelines 只消费成功 GitHub Actions run 生成的不可变制品，仅执行 CD，不
checkout 源码、不构建、不测试。

The machine-readable authority is [`repository-policy.yaml`](../repository-policy.yaml).
Multi-repository topology and compatibility pins belong to
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).

机器可读权威位于 [`repository-policy.yaml`](../repository-policy.yaml)，多仓拓扑与
兼容性 pin 由 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace)
维护。
