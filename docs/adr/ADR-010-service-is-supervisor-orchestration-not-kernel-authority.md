# ADR-010: Service is a Supervisor Orchestration Abstraction, Not a Kernel Semantic Authority Entity

- **Status**: ACCEPTED
- **Date**: 2026-08-27

## Context
Hosting long-running workloads (e.g. model workers, HTTP/gRPC services) requires generic process management mechanisms: working directory configuration, multi-strategy readiness probing (`ProcessAlive`, `TcpSocket`, `HttpGet`, `WorkerControl`), deterministic exponential restart backoff, and endpoint publication synchronization.

In introducing `ServiceSpec`, `ServiceState`, `ServiceStatus`, `ServiceEvent`, and `ServiceSupervisor`, there is a risk of future architectural erosion where developers might observe these names and attempt to promote `Service` into a 10th authoritative Kernel semantic entity by introducing `ServiceId`, `ServiceRepository`, `ServiceLedger`, `ServiceLease`, or persistent service tables.

## Decision
1. **Semantic Model Frozen**: The Kernel semantic model in `cy-kernel-contract` is strictly frozen to the canonical nouns: `Worker`, `Operation`, `Endpoint`, `EndpointGrant`, `Lease`, `Event`, `Principal`, `Provider`, and `Capability`.
2. **Service is Orchestration Only**: `Service` (`ServiceSpec`, `ServiceSupervisor`) is strictly a **daemon-level generic process supervision abstraction**, NOT an authoritative Kernel domain entity.
3. **No Kernel Authority Extensions**:
   - **NO** authoritative `ServiceId` resource in `cy-kernel-contract`.
   - **NO** `ServiceRepository`, `ServiceLedger`, or persistent `Service` tables in Kernel authority.
   - **NO** independent authoritative `Service` event stream in the gRPC/UDS authority ledger.
4. **Primitive Composition**: `ServiceSupervisor` drives existing Kernel primitives: `LaunchPlan`, `ProcessRuntime` / `SandboxBackend`, `CleanupReport`, and `semantic::Endpoint`.

## Why
1. **Prevents Conceptual Bloat**: Avoids duplicating the `Worker` and `Operation` execution lifecycle with redundant "Service" ledger persistence.
2. **Clear Boundary**: The Kernel authority continues to govern identity, leases, fence tokens, and physical capabilities, while the supervisor manages process lifecycle loops, readiness probing, and backoff timing.
3. **Future Drift Prevention**: Establishes an explicit boundary so future contributors do not build parallel ledger state for long-running processes.

## Consequences
- Long-running application workloads are represented to the supervisor via `ServiceSpec` and executed over standard `ProcessRuntime` and `LaunchPlan`.
- Service health and readiness translate directly into standard `semantic::Endpoint` publication and revocation.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-010：Service 是 supervisor 编排抽象，不是 Kernel 语义权威实体

- **状态**：ACCEPTED
- **日期**：2026-08-27

## 背景
托管长期运行的工作负载（例如模型 worker、HTTP/gRPC 服务）需要通用进程管理机制：工作目录配置、多策略就绪探测（`ProcessAlive`、`TcpSocket`、`HttpGet`、`WorkerControl`）、确定性指数重启退避，以及端点发布同步。

引入 `ServiceSpec`、`ServiceState`、`ServiceStatus`、`ServiceEvent` 和 `ServiceSupervisor` 后，存在架构逐渐偏移的风险：开发者看到这些名称，可能会试图通过新增 `ServiceId`、`ServiceRepository`、`ServiceLedger`、`ServiceLease` 或持久化 service 表，将 Service 升格为第十个权威 Kernel 语义实体。

## 决策
1. **语义模型冻结**：`cy-kernel-contract` 中的 Kernel 语义模型严格限于规范名词：`Worker`、`Operation`、`Endpoint`、`EndpointGrant`、`Lease`、`Event`、`Principal`、`Provider` 和 `Capability`。
2. **Service 仅用于编排**：`Service`（`ServiceSpec`、`ServiceSupervisor`）严格来说是 daemon 层的通用进程监管抽象，**不是** Kernel 的权威领域实体。
3. **不得扩展 Kernel 权威模型**：
   - `cy-kernel-contract` 中**不得**定义权威 `ServiceId` 资源。
   - Kernel authority 中**不得**添加 `ServiceRepository`、`ServiceLedger` 或持久化 `Service` 表。
   - gRPC/UDS authority ledger 中**不得**增加独立的权威 `Service` 事件流。
4. **组合现有基元**：`ServiceSupervisor` 驱动现有 Kernel 基元：`LaunchPlan`、`ProcessRuntime` / `SandboxBackend`、`CleanupReport` 和 `semantic::Endpoint`。

## 原因
1. **避免概念膨胀**：不重复持久化 `Worker` 和 `Operation` 的执行生命周期，也不引入冗余的“Service”账本。
2. **明确边界**：Kernel authority 继续管理身份、租约、fence token 和物理能力；supervisor 管理进程生命周期循环、就绪探测和退避计时。
3. **防止未来漂移**：明确边界，避免贡献者为长期运行进程另建并行账本状态。

## 后果
- 长期运行的应用工作负载通过 `ServiceSpec` 提交给 supervisor，并经标准 `ProcessRuntime` 和 `LaunchPlan` 执行。
- Service health 和 readiness 会直接转换为标准 `semantic::Endpoint` 的发布与撤销。
