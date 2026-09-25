# ADR-003: Operation Is a Generic Execution Primitive

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
We needed a unified mechanism to represent batch tasks, setup scripts, data downloads, and fine-tuning steps across heterogeneous clusters.

## Decision
An `Operation` is defined as an ephemeral, time-bounded execution unit managed by the Kernel supervisor within an isolated sandbox. It produces an exit status, logs, and optional output artifacts.

## Why
Whether downloading a model checkpoint, building a Docker image, or running a 10-epoch training pass, the Kernel executes them as generic Operations governed by Lease tokens and signal propagation.

## Alternatives Considered
- *Domain-Specific Job Types*: Custom `TrainJob`, `EvalJob`, `BuildJob` in the Kernel. Rejected to prevent domain logic leakage.

## Consequences
- High-level multi-phase jobs are compiled into a sequence of `PlanStep` attempts executed as Operations.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-003：Operation 是通用执行基元

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
我们需要一种统一机制，在异构集群中表示批处理任务、初始化脚本、数据下载和微调步骤。

## 决策
`Operation` 定义为由 Kernel supervisor 管理、在隔离沙箱中运行、有时间限制的临时执行单元。它会产出退出状态、日志和可选输出制品。

## 原因
无论下载模型 checkpoint、构建 Docker 镜像还是运行 10 个 epoch 的训练，Kernel 都将其作为由 Lease token 和信号传播机制管理的通用 Operation 执行。

## 考虑过的替代方案
- *领域专属 Job 类型*：在 Kernel 中自定义 `TrainJob`、`EvalJob`、`BuildJob`。为避免领域逻辑泄漏而被拒绝。

## 后果
- 高层多阶段任务会编译为一系列 `PlanStep` 尝试，并作为 Operation 执行。
