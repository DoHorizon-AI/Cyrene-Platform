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
