# ADR-008: GitHub Is Source of Truth for Source Code

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Teams evaluated whether to maintain public code on GitHub and private code exclusively on Azure Repos.

## Decision
GitHub is the single source-of-truth for all source code repositories (both Public and Private). Azure DevOps serves as the internal delivery and operations plane.

## Why
Consolidating source control on GitHub standardizes developer tooling, issue tracking, and PR reviews while leveraging Azure DevOps for secure enterprise CI/CD and deployments.

## Alternatives Considered
- *Split Source Control (GitHub + Azure Repos)*: Rejected due to git history synchronization friction and developer overhead.

## Consequences
- CI pipelines in Azure DevOps checkout repositories hosted on GitHub.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-008：GitHub 是源代码的事实来源

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
团队曾评估是否在 GitHub 上维护公开代码、仅在 Azure Repos 中维护私有代码。

## 决策
所有源代码仓库（公开和私有）都以 GitHub 为单一事实来源。Azure DevOps 作为内部交付与运维平台使用。

## 原因
将源代码管理集中到 GitHub，可统一开发者工具、问题跟踪和 PR 审查；同时利用 Azure DevOps 进行安全的企业 CI/CD 和部署。

## 考虑过的替代方案
- *拆分源代码管理（GitHub + Azure Repos）*：因 Git 历史同步困难和开发者维护负担而被拒绝。

## 后果
- Azure DevOps 中的 CI pipeline 从托管在 GitHub 的仓库检出代码。
