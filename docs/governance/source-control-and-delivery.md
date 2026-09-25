# Source Control & Delivery Responsibility Model

This document establishes the official separation of responsibilities between **GitHub** and **Azure DevOps**.

---

## 1. GitHub: The Canonical Source of Truth

**GitHub is the single source-of-truth for all source code repositories.**

```text
┌─────────────────────────────────────────────────────────────┐
│                           GITHUB                            │
│                  Source-of-Truth for Code                   │
├──────────────────────────────┬──────────────────────────────┤
│     PUBLIC REPOSITORIES      │     PRIVATE REPOSITORIES     │
│   • Cyrene-Platform          │   • Cyrene-Commercial-Plugins│
│   • Cyrene-Plugins-Official           │   • Cyrene-Enterprise        │
│   • Public Services          │   • Internal Operations      │
└──────────────────────────────┴──────────────────────────────┘
```

- Every repository (whether Public or Private) maintains its authoritative Git history, pull requests, issue tracking, and code reviews on GitHub.
- There is **no** split where public code lives on GitHub and private code lives on Azure Repos.

---

## 2. Azure DevOps: Internal Delivery & Operations Plane

**Azure DevOps is the internal operations and enterprise delivery plane.**

```text
┌─────────────────────────────────────────────────────────────┐
│                        AZURE DEVOPS                         │
│             Internal Delivery & Operations Plane            │
├─────────────────────────────────────────────────────────────┤
│   • Multi-stage Enterprise Pipelines & Release Gates        │
│   • Private Package Feeds (Azure Artifacts / NuGet / PyPI)  │
│   • Service Connections, Cloud Subscriptions & HSM Signing  │
│   • Environment Deployment Approvals & Fleet Telemetry      │
└─────────────────────────────────────────────────────────────┘
```

- Azure DevOps checks out code from GitHub via service connections to run continuous integration, security scans, private package builds, and fleet deployments.
---

<!-- Chinese Translation / 中文翻译 -->

# 源代码管理与交付职责模型

本文规定 **GitHub** 和 **Azure DevOps** 之间的正式职责划分。

---

## 1. GitHub：规范源码事实来源

**所有源代码仓库都以 GitHub 为单一事实来源。**

```text
┌─────────────────────────────────────────────────────────────┐
│                           GITHUB                            │
│                       源码事实来源                          │
├──────────────────────────────┬──────────────────────────────┤
│          公开仓库            │          私有仓库            │
│   • Cyrene-Platform          │   • Cyrene-Commercial-Plugins│
│   • Cyrene-Plugins-Official  │   • Cyrene-Enterprise        │
│   • Public Services          │   • Internal Operations      │
└──────────────────────────────┴──────────────────────────────┘
```

- 每个仓库（公开或私有）都在 GitHub 上维护规范 Git 历史、pull request、问题跟踪和代码评审。
- 不将公开代码放在 GitHub、私有代码放在 Azure Repos，不采用这种拆分源码管理方式。

---

## 2. Azure DevOps：内部交付与运维平台

**Azure DevOps 是内部运维与企业交付平台。**

```text
┌─────────────────────────────────────────────────────────────┐
│                        AZURE DEVOPS                         │
│                    内部交付与运维平台                       │
├─────────────────────────────────────────────────────────────┤
│   • 多阶段企业 Pipeline 与发布 Gate                         │
│   • 私有 Package Feed（Azure Artifacts / NuGet / PyPI）     │
│   • Service Connection、云订阅和 HSM 签名                   │
│   • 环境部署审批与 Fleet 遥测                                │
└─────────────────────────────────────────────────────────────┘
```

- Azure DevOps 通过 Service Connection 从 GitHub 检出源码，以运行持续集成、安全扫描、私有 package 构建和 Fleet 部署。
