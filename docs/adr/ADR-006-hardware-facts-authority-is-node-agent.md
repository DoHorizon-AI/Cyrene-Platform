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
