# ADR-006: HardwareFacts Authority Is Node Agent

- **Status**: SUPERSEDED / HISTORICAL
- **Date**: 2026-08-26
- **Superseded by**: `ADR-HARDWARE-ADAPTER-BOUNDARY.md` and
  `docs/architecture/system-adapter.md`

> This ADR records an earlier Node Agent-centered design and is retained for
> decision history. It is not the current hardware-facts authority model.
>
> 本 ADR 记录早期以 Node Agent 为中心的设计，仅保留作决策历史；它不是当前的
> 硬件事实权威模型。

## Context
Different components previously ran ad-hoc `nvidia-smi` or PyTorch CUDA checks, producing inconsistent telemetry and missing non-NVIDIA accelerators.

## Historical decision

The earlier decision made the Platform `Node Agent` the single source-of-truth
for physical node hardware discovery (`HardwareFacts`).

## Why
Guarantees consistent, cached, and vendor-agnostic hardware facts across all schedulers, evaluators, and preflight checks.

## Alternatives Considered
- *Per-Plugin Hardware Probes*: Let each plugin inspect `/dev` or invoke vendor tools directly. Rejected due to permission issues and inconsistent reporting.

## Consequences
- Preflight and evaluators accept `HardwareFacts` as input arguments during estimation.

## Current disposition | 当前处置

The current architecture separates host-system and vendor facts into explicit
Adapter Hosts. `SystemAdapter` owns target-OS facts and generic CPU/RAM resource
projections; Hardware Adapters own vendor inventory, topology, bindings, and
health. Kernel validates and stores normalized observations, while Node Agent
exchanges typed control and runtime evidence. Node Agent is not the driver-query
authority and must not be documented as one.

当前架构将主机系统事实与厂商事实拆分到明确的 Adapter Host：`SystemAdapter` 拥有
目标 OS 事实和通用 CPU/RAM 资源投影；Hardware Adapter 拥有厂商 inventory、topology、
binding 与 health。Kernel 校验并保存标准化观测，Node Agent 负责 typed control 和运行
证据交换。Node Agent 不是驱动查询权威，文档不能再把它描述成唯一硬件发现源。
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-006：HardwareFacts 权威方是 Node Agent（已取代 / 历史记录）

- **状态**：SUPERSEDED / HISTORICAL
- **日期**：2026-08-26
- **取代文档**：`ADR-HARDWARE-ADAPTER-BOUNDARY.md` 和 `docs/architecture/system-adapter.md`

> 本 ADR 记录早期以 Node Agent 为中心的设计，仅作为决策历史保留。它不是当前硬件事实权威模型。

## 背景
过去，不同组件各自运行 `nvidia-smi` 或 PyTorch CUDA 检查，导致遥测结果不一致，也遗漏了 NVIDIA 以外的加速器。

## 历史决策
早期决策指定 Platform `Node Agent` 为物理节点硬件发现（`HardwareFacts`）的单一事实来源。

## 原因
确保所有调度器、评估器和 preflight 检查使用一致、缓存且与厂商无关的硬件事实。

## 考虑过的替代方案
- *各 Plugin 独立探测硬件*：让每个 plugin 检查 `/dev` 或直接调用厂商工具。因权限问题和报告不一致而被拒绝。

## 后果
- preflight 与评估器在估算期间接收 `HardwareFacts` 作为输入参数。

## 当前处置

当前架构将主机系统事实和厂商事实交给不同的 Adapter Host。`SystemAdapter` 拥有目标操作系统事实及通用 CPU/RAM 资源投影；Hardware Adapter 拥有厂商 inventory、topology、binding 和 health。Kernel 校验并保存规范化观测，Node Agent 交换类型化控制与运行时证据。Node Agent 不是驱动查询权威，文档不得将其描述为此权威。
