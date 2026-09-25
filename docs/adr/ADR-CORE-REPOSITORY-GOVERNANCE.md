# ADR-CORE-REPOSITORY-GOVERNANCE: Core and Advanced-Service Boundaries

- Status: Approved / Normative
- Date: 2026-08-10
- Supersedes: repository-boundary alternatives described before the split-repository decision

## Decision

CYRENE uses one reusable Core repository and one independent repository for each
first-party service. The Core repository contains the Rust host runtime, public
contracts, framework boundary, examples, conformance tooling, and governance
documentation. It does not contain Catalyst, Yield, Reactor, Exchange,
Navigator, Echo, vendor runtimes, enterprise bundles, or product UI source.

Each Product repository owns its service implementation, Product manifest,
deployment assets, and migration source. Cyrene-Plugins-Official owns reusable
capability manifests, payload contracts, packages, SDKs, implementations, and
TCKs. Consumers record exact released Platform dependencies in their own lock
or build configuration.

## Branch and release policy

- `develop-kernel` is the Rust Kernel/Node Runtime development branch.
- `develop` is the reviewed cross-language integration branch.
- `main` is the release branch and accepts only pull requests whose head branch
  is `develop`.
- `develop` and `main` require pull requests, one independent approval, current
  required checks, resolved conversations, and no force-push or deletion.
- The main release PR additionally requires a release-evidence-verified label
  and a linked verification record.
- Administrators are subject to the same protected-branch rules.

## Consequences

Core can be built and tested without checking out any first-party service.
Cross-repository changes are coordinated through versioned contracts, TCK
artifacts, signed packages, and the service's `core.lock` rather than source
imports or Git submodules.

This ADR does not create Core v1, configure a deployment environment, or migrate
the six services. Those are later phases with their own acceptance gates.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-CORE-REPOSITORY-GOVERNANCE：Core 与 Advanced Service 边界

- 状态：Approved / Normative
- 日期：2026-08-10
- 取代内容：拆分仓库决策之前提出的仓库边界替代方案

## 决策

CYRENE 使用一个可复用的 Core 仓库，并为每个第一方 Service 建立一个独立仓库。Core 仓库包含 Rust host runtime、公开契约、Framework 边界、示例、一致性工具和治理文档。它不包含 Catalyst、Yield、Reactor、Exchange、Navigator、Echo、厂商 runtime、企业 bundle 或 Product UI 源码。

每个 Product 仓库拥有自身的 Service 实现、Product manifest、部署资源和迁移源码。Cyrene-Plugins-Official 拥有可复用的 capability manifest、payload contract、package、SDK、实现和 TCK。Consumer 在自己的 lock 或构建配置中记录精确发布版本的 Platform 依赖。

## 分支与发布政策

- `develop-kernel` 是 Rust Kernel/Node Runtime 开发分支。
- `develop` 是经过审查的跨语言集成分支。
- `main` 是发布分支，只接受 head branch 为 `develop` 的 pull request。
- `develop` 和 `main` 都要求使用 pull request、至少一项独立批准、当前必需检查全部通过、解决 review conversation，且禁止 force-push 或删除分支。
- 合并至 `main` 的 release PR 还必须带有 `release-evidence-verified` label，并链接验证记录。
- 管理员也受同一组受保护分支规则约束。

## 后果

Core 无需检出任何第一方 Service 即可构建和测试。跨仓变更通过版本化契约、TCK 制品、签名 package 和 Service 的 `core.lock` 协调，而不是靠导入源码或 Git submodule。

本 ADR 不创建 Core v1、不配置部署环境，也不迁移六个 Service。这些属于后续阶段，各自有验收 gate。
