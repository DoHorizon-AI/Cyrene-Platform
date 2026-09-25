# ADR-009: Shared Infrastructure and Tooling Live in Platform

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Dockerfiles, Compose stacks, Kubernetes manifests, CI governance scripts, and migration tooling were previously scattered across plugin and service directories.

## Decision
Shared infrastructure assets (`infrastructure/`) and shared engineering governance tooling (`tooling/`) are consolidated directly inside `Cyrene-Platform`.

## Why
Provides a single canonical location for cluster deployments, observability dashboards, boundary guards, and workspace tooling.

## Alternatives Considered
- *Standalone Tooling / Infra Repositories*: Rejected to avoid repository proliferation and cross-repository synchronization lag.

## Consequences
- Platform CI validates infrastructure syntax and enforces architecture governance across the workspace.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-009：共享基础设施与工具归 Platform 所有

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
Dockerfile、Compose stack、Kubernetes manifest、CI 治理脚本和迁移工具过去分散在 plugin 与 service 目录中。

## 决策
共享基础设施资源（`infrastructure/`）和共享工程治理工具（`tooling/`）统一放在 `Cyrene-Platform` 中。

## 原因
这为集群部署、可观测性 dashboard、边界检查和 workspace 工具提供唯一规范位置。

## 考虑过的替代方案
- *独立工具 / Infra 仓库*：因会增加仓库数量并造成跨仓同步延迟而被拒绝。

## 后果
- Platform CI 校验基础设施语法，并在 workspace 范围实施架构治理。
