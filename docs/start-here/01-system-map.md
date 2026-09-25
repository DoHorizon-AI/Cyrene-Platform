# Cyrene System Map

This document details the layered architectural topology of Cyrene and defines the strict boundaries between **Platform**, **Kernel Core**, **Services**, and **Plugins**.

---

## Architectural Layers

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                           PRODUCT SERVICES LAYER                        │
│  Cyrene-Yield (Training)   Cyrene-Reactor (Serving)   Cyrene-Exchange   │
│  • Owns Product State      • Owns Scaling Policy     • Owns API Routing │
│  • Owns TrainingRun        • Owns Deployment State   • Owns Upstreams   │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │ (Requests & Declarative Plans)
┌────────────────────────────────────▼────────────────────────────────────┐
│                          CYRENE-PLATFORM LAYER                          │
│                                                                         │
│   ┌─────────────────────────────────────────────────────────────────┐   │
│   │                         CONTROL PLANE                           │   │
│   │   • ExecutionPlan, PlanStep, Attempt, Generation, Reconciler    │   │
│   │   • Artifact Contract & Provider   • Capability Package Control │   │
│   └────────────────────────────────┬────────────────────────────────┘   │
│                                    │ (Generic Operations & Leases)      │
│   ┌────────────────────────────────▼────────────────────────────────┐   │
│   │                          KERNEL CORE                            │   │
│   │   • Node Resource Leases & Fencing (Win32 Job / POSIX Cgroups)  │   │
│   │   • WorkerControl & Process Supervisor (Generic Lifecycle)      │   │
│   │   • HardwareFacts Telemetry Authority (Node Agent)              │   │
│   └────────────────────────────────┬────────────────────────────────┘   │
└────────────────────────────────────┼────────────────────────────────────┘
                                     │ (Worker IPC / Inline Invocation)
┌────────────────────────────────────▼────────────────────────────────────┐
│                              PLUGIN LAYER                               │
│  Cyrene-Plugins-Official (Official Community & First-Party Capabilities)         │
│  • Model Analyzers    • Storage & Memory     • Tool & IM Connectors     │
│  • Compatibility Rules• Gateway Runtimes     • Environment Builders     │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Key Invariants & Distinctions

### 1. Platform != Kernel
- **Kernel Core** (`kernel/`): A lightweight, high-performance generic execution substrate written in Rust. It only understands processes, CPU/GPU resource allocations, time-bounded leases, execution sandboxes, and signal dispatch. It does **not** understand PyTorch, CUDA kernels, model tokens, or training epochs.
- **Platform** (`Cyrene-Platform`): The broader foundational repository containing the Kernel, generic Control Plane mechanisms, client SDKs (`cyrene_artifacts`, `cyrene_preflight`), shared infrastructure, and engineering tooling.

### 2. Services Own Product Semantics
- **Cyrene-Yield** owns the concept of a `TrainingRun`, dataset paths, hyperparameters, epoch progress, checkpoint schedules, and evaluation metrics.
- **Cyrene-Reactor** owns `Deployment` desired state, replica counts, KV-cache scaling policies, and inference metrics.
- **Cyrene-Exchange** owns gateway routing rules, rate limits, and model provider failover policies.

### 3. Plugins Implement Replaceable Capabilities
- A Plugin is an implementation of a versioned **Capability contract** (e.g. `model.analyzer.v1`, `storage.provider.v1`, `gateway.runtime.v1`).
- Plugins are executed either out-of-process via `WorkerControl`, as standalone jobs via `Operation`, or inline through language SDKs.
- Services interact with Plugins via **Capability Handshakes and Resolvers**, never by hardcoded static imports of plugin implementation internals.
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene 系统图

本文详细说明 Cyrene 的分层架构拓扑，并定义 **Platform**、**Kernel Core**、**Services** 与 **Plugins** 之间的严格边界。

---

## 架构分层

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                           PRODUCT SERVICES LAYER                        │
│  Cyrene-Yield（Training）  Cyrene-Reactor（Serving）  Cyrene-Exchange   │
│  • 拥有 Product 状态      • 拥有扩缩容策略          • 拥有 API 路由     │
│  • 拥有 TrainingRun       • 拥有 Deployment 状态    • 拥有 Upstream     │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │（请求与声明式计划）
┌────────────────────────────────────▼────────────────────────────────────┐
│                          CYRENE-PLATFORM LAYER                          │
│                                                                         │
│   ┌─────────────────────────────────────────────────────────────────┐   │
│   │                         CONTROL PLANE                           │   │
│   │   • ExecutionPlan、PlanStep、Attempt、Generation、Reconciler     │   │
│   │   • Artifact Contract & Provider   • Capability Package Control │   │
│   └────────────────────────────────┬────────────────────────────────┘   │
│                                    │（通用 Operation 与 Lease）         │
│   ┌────────────────────────────────▼────────────────────────────────┐   │
│   │                          KERNEL CORE                            │   │
│   │   • 节点资源 Lease 与 fencing（Win32 Job / POSIX Cgroup）      │   │
│   │   • WorkerControl 与进程监管器（通用生命周期）                  │   │
│   │   • HardwareFacts 遥测权威（Node Agent）                       │   │
│   └────────────────────────────────┬────────────────────────────────┘   │
└────────────────────────────────────┼────────────────────────────────────┘
                                     │（Worker IPC / 进程内调用）
┌────────────────────────────────────▼────────────────────────────────────┐
│                              PLUGIN LAYER                               │
│  Cyrene-Plugins-Official（官方社区能力与第一方能力）                    │
│  • Model Analyzer • Storage & Memory • Tool & IM Connector             │
│  • Compatibility Rule • Gateway Runtime • Environment Builder          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 关键不变量与区别

### 1. Platform != Kernel
- **Kernel Core**（`kernel/`）：使用 Rust 编写的轻量、高性能通用执行基础设施。它只处理进程、CPU/GPU 资源分配、有时限的租约、执行沙箱和信号分发。它不理解 PyTorch、CUDA kernel、模型 token 或训练 epoch。
- **Platform**（`Cyrene-Platform`）：更广义的基础仓库，包含 Kernel、通用 Control Plane 机制、客户端 SDK（`cyrene_artifacts`、`cyrene_preflight`）、共享基础设施和工程工具。

### 2. Services 拥有 Product 语义
- **Cyrene-Yield** 拥有 `TrainingRun`、数据集路径、超参数、epoch 进度、checkpoint 计划和评估指标。
- **Cyrene-Reactor** 拥有 `Deployment` 期望状态、副本数量、KV-cache 扩缩容策略和推理指标。
- **Cyrene-Exchange** 拥有网关路由规则、速率限制和模型 provider 故障切换策略。

### 3. Plugins 实现可替换能力
- Plugin 是版本化 **Capability 契约**（例如 `model.analyzer.v1`、`storage.provider.v1`、`gateway.runtime.v1`）的一种实现。
- Plugins 可以通过 `WorkerControl` 在进程外执行、作为独立 `Operation` job 执行，或通过语言 SDK 在进程内调用。
- Services 通过 **Capability Handshake 与 Resolver** 和 Plugins 交互，不能静态硬编码导入 plugin 实现内部模块。
