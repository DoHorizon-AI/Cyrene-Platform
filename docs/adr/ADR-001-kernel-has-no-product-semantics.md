# ADR-001: Kernel Has No Product Semantics

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Initial system sketches placed training hyperparameter checks and serving route lookups inside the Rust kernel runtime. This caused tight coupling and made Kernel crashes cascade to unrelated services.

## Decision
The Kernel core shall remain a pure generic execution substrate. It manages OS processes, memory limits, cgroups/Job Objects, and time-bounded resource leases. It shall contain zero product-specific concepts (no TrainingSpec, no prompt templates, no token counters, no billing).

## Why
1. **Fault Isolation**: Product crashes do not compromise the node-level process supervisor.
2. **Minimal Privileges**: Sandboxing and lease enforcement require elevated permissions; domain logic does not.
3. **Generic Reuse**: Identical primitives serve training, inference, and data indexing.

## Alternatives Considered
- *Monolithic AI Kernel*: Embed PyTorch C++ / vLLM bindings directly in the Kernel. Rejected due to security, stability, and high coupling.

## Consequences
- Product services must compile domain intent into declarative `ExecutionPlan` steps that the Kernel executes via generic `WorkerControl` and `Operation` primitives.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-001：Kernel 不承载 Product 语义

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
最初的系统设计草图将训练超参数校验和服务路由查询放入 Rust kernel runtime。这造成了紧密耦合，也使 Kernel 故障会扩散到无关服务。

## 决策
Kernel core 必须保持为纯粹的通用执行基础设施。它管理操作系统进程、内存限制、cgroup/Job Object 和有时间限制的资源租约。它不得包含任何特定于产品的概念（例如 TrainingSpec、prompt 模板、token 计数器或计费）。

## 原因
1. **故障隔离**：Product 故障不会危及节点级进程监管器。
2. **最小权限**：沙箱和租约强制执行需要提升权限；领域逻辑不需要。
3. **通用复用**：同一组基元可服务训练、推理和数据索引。

## 考虑过的替代方案
- *单体 AI Kernel*：将 PyTorch C++ / vLLM 绑定直接嵌入 Kernel。因安全性、稳定性和高度耦合而被拒绝。

## 后果
- Product 服务必须将领域意图编译为声明式 `ExecutionPlan` 步骤，再由 Kernel 通过通用 `WorkerControl` 和 `Operation` 基元执行。
