# Continuous Integration (CI) Architecture

This guide explains what CI runs on pull requests, how checks are structured, and how CI tiers operate.

---

## 1. The Four CI Execution Tiers

```text
┌─────────────────────────────────────────────────────────────┐
│  Tier 0: Static, Docs & Governance (Runs on EVERY PR)       │
│  • Markdown link validator, boundary checker (< 10s)        │
├─────────────────────────────────────────────────────────────┤
│  Tier 1: Fast Unit, Build & Conformance (Standard PR)       │
│  • Python SDK pytest, Cargo test --locked, TCK (< 2m)       │
├─────────────────────────────────────────────────────────────┤
│  Tier 2: Container & Integration (Merge / Nightly)          │
│  • Multi-worker integration, Docker image builds            │
├─────────────────────────────────────────────────────────────┤
│  Tier 3: Accelerator Hardware CI (Internal / Scheduled)     │
│  • Real NVIDIA / TPU / Ascend physical node execution       │
└─────────────────────────────────────────────────────────────┘
```

> [!IMPORTANT]
> **Tier 3 (Accelerator Hardware) is NEVER required for standard public PR merges.** Public PRs run strictly on standard CPU runners with mock hardware facts.

---

## 2. Path-Aware Workflow Optimization

To avoid wasteful compute, workflows use path filtering:
- **Docs-only PRs** (`docs/**`, `*.md`): Execute `ci / docs` and `ci / governance`, safely skipping native Rust/C++ compilation.
- **SDK changes** (`sdk/python/**`): Trigger Python test matrices and preflight tests.
- **Kernel changes** (`kernel/**`, `Cargo.*`): Trigger Rust formatting, clippy, and unit tests.
---

<!-- Chinese Translation / 中文翻译 -->

# 持续集成（CI）架构

本指南说明 pull request 会运行哪些 CI、检查如何组织，以及各 CI 层级如何工作。

---

## 1. 四个 CI 执行层级

```text
┌─────────────────────────────────────────────────────────────┐
│ Tier 0：静态检查、文档与治理（每个 PR 都运行）              │
│ • Markdown 链接校验、边界检查（< 10 秒）                    │
├─────────────────────────────────────────────────────────────┤
│ Tier 1：快速单元测试、构建与一致性检查（标准 PR）           │
│ • Python SDK pytest、cargo test --locked、TCK（< 2 分钟）   │
├─────────────────────────────────────────────────────────────┤
│ Tier 2：容器与集成（合并时 / 夜间）                         │
│ • 多 worker 集成、Docker 镜像构建                            │
├─────────────────────────────────────────────────────────────┤
│ Tier 3：加速器硬件 CI（内部 / 定时）                        │
│ • 在真实 NVIDIA / TPU / Ascend 物理节点上执行               │
└─────────────────────────────────────────────────────────────┘
```

> [!IMPORTANT]
> **Tier 3（加速器硬件）绝不是标准公开 PR 合并的必需条件。**公开 PR 严格使用标准 CPU runner 和模拟硬件事实。

---

## 2. 按路径优化 workflow

为避免浪费计算资源，workflow 会按路径过滤：
- **仅文档 PR**（`docs/**`、`*.md`）：运行 `ci / docs` 和 `ci / governance`，安全跳过原生 Rust/C++ 编译。
- **SDK 改动**（`sdk/python/**`）：触发 Python 测试矩阵和 preflight 测试。
- **Kernel 改动**（`kernel/**`、`Cargo.*`）：触发 Rust 格式检查、clippy 和单元测试。
