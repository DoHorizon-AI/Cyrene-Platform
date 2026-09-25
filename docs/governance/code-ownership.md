# Code Ownership Guidelines

This document outlines logical component ownership across Cyrene repositories.

---

## Logical Ownership Areas

| Component / Subsystem | Primary Focus Area | Responsible Role | GitHub Team (Mapping TODO) |
|---|---|---|---|
| **Platform Kernel** (`kernel/`) | Rust node supervisor, process isolation, cgroups, leases | Kernel Maintainers | `@dohorizon/kernel-team` *(TODO)* |
| **Framework & SDK** (`framework/`, `sdk/`) | Reconciler, ExecutionPlan, Attempt, Artifacts | Platform Team | `@dohorizon/platform-team` *(TODO)* |
| **Cyrene-Yield** | Training orchestration, TrainingRun, hyperparameters | Training Leads | `@dohorizon/yield-team` *(TODO)* |
| **Cyrene-Reactor** | Serving orchestration, deployment scaling, inference | Serving Leads | `@dohorizon/reactor-team` *(TODO)* |
| **Cyrene-Plugins-Official** | Plugin manifests, official catalog, capability adapters | Ecosystem Team | `@dohorizon/plugins-team` *(TODO)* |
| **Architecture Governance** (`tooling/ci/`) | Service boundaries, dependency rules, CI guards | Architecture Guild | `@dohorizon/architecture` *(TODO)* |

> [!NOTE]
> Exact GitHub team handles will be mapped when organization-level identity provider synchronization is activated.
---

<!-- Chinese Translation / 中文翻译 -->

# 代码归属指南

本文概述 Cyrene 各仓库中逻辑组件的责任归属。

---

## 逻辑归属范围

| 组件 / 子系统 | 主要职责范围 | 负责角色 | GitHub Team（映射待定） |
|---|---|---|---|
| **Platform Kernel**（`kernel/`） | Rust 节点监管、进程隔离、cgroup、Lease | Kernel Maintainer | `@dohorizon/kernel-team`（待定） |
| **Framework 与 SDK**（`framework/`、`sdk/`） | Reconciler、ExecutionPlan、Attempt、Artifact | Platform Team | `@dohorizon/platform-team`（待定） |
| **Cyrene-Yield** | 训练编排、TrainingRun、超参数 | Training Lead | `@dohorizon/yield-team`（待定） |
| **Cyrene-Reactor** | 服务编排、部署扩缩容、推理 | Serving Lead | `@dohorizon/reactor-team`（待定） |
| **Cyrene-Plugins-Official** | Plugin manifest、官方目录、capability adapter | Ecosystem Team | `@dohorizon/plugins-team`（待定） |
| **架构治理**（`tooling/ci/`） | Service 边界、依赖规则、CI 检查 | Architecture Guild | `@dohorizon/architecture`（待定） |

> [!NOTE]
> 组织级身份提供方同步启用后，才会映射准确的 GitHub Team handle。
