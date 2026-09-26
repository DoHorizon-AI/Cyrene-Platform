# ADR-005: WorkerControl Is an Execution Boundary

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Long-running AI daemons (like inference servers or vector search workers) need lifecycle supervision without exposing unrestricted process spawning.

## Decision
`WorkerControl` is the authoritative platform boundary for supervising long-running out-of-process workers. It handles startup health handshakes, Lease renewal, and clean process tree termination via OS Job Objects / Cgroups.

## Why
AI workers frequently spawn CUDA child processes. Unsupervised raw process spawning leads to leaked GPU VRAM and zombie processes.

## Alternatives Considered
- *Raw `subprocess.Popen` in Services*: Rejected because service-level crashes leave orphaned child workers running indefinitely on GPU nodes.

## Consequences
- All long-running service backends must be launched and monitored via `WorkerControl`.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-005：WorkerControl 是执行边界

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
长期运行的 AI daemon（例如推理服务器或向量搜索 worker）需要生命周期监管，但不能因此开放不受限制的进程启动能力。

## 决策
`WorkerControl` 是监管长期运行进程外 worker 的 Platform 权威边界。它负责启动健康握手、Lease 续期，以及通过 OS Job Object / Cgroup 干净地终止整个进程树。

## 原因
AI worker 经常会启动 CUDA 子进程。若直接启动且无人监管，GPU VRAM 会泄漏，也会留下僵尸进程。

## 考虑过的替代方案
- *Service 中直接使用 `subprocess.Popen`*：因 Service 故障会使孤儿子 worker 无限期运行在 GPU 节点上而被拒绝。

## 后果
- 所有长期运行的服务后端都必须通过 `WorkerControl` 启动和监控。
