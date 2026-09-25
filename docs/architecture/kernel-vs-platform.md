# Architecture Deep-Dive: Kernel vs. Platform

A foundational architectural invariant in Cyrene is: **Platform != Kernel**.

---

## Why Is the Kernel Stripped of Product Semantics?

```text
┌─────────────────────────────────────────────────────────────┐
│                      CYRENE-PLATFORM                        │
│                                                             │
│   ┌─────────────────────────────────────────────────────┐   │
│   │                    CONTROL PLANE                    │   │
│   │  Understands: ExecutionPlan, Artifacts, Spec, Reconciler│
│   └──────────────────────────┬──────────────────────────┘   │
│                              │ (Generic Lease / Operation)  │
│   ┌──────────────────────────▼──────────────────────────┐   │
│   │                     KERNEL CORE                     │   │
│   │  Understands: Processes, CPU/GPU Leases, Sandboxes  │   │
│   │  DOES NOT Understand: PyTorch, Tokens, Checkpoints  │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

1. **Failure Domain Isolation**: The Kernel runs as a node supervisor and process manager. If high-level training logic or prompt templating crashes, the Kernel process must remain resilient and capable of cleaning up child process trees via Win32 Job Objects or POSIX Cgroups.
2. **Minimal Trust & Privilege Surface**: The Kernel manages hardware leases and OS sandboxes. Keeping product code out of the Kernel ensures that security boundaries are clean and minimal.
3. **Generic Reuse**: The exact same Kernel primitives (`Worker`, `Operation`, `Endpoint`, `Lease`, `Fence`) power training jobs (Yield), model serving instances (Reactor), and vector retrieval workers (Memory Plugin).
---

<!-- Chinese Translation / 中文翻译 -->

# 架构深入解析：Kernel 与 Platform

Cyrene 的基础架构不变量是：**Platform != Kernel**。

---

## 为什么 Kernel 不承载 Product 语义？

```text
┌─────────────────────────────────────────────────────────────┐
│                      CYRENE-PLATFORM                        │
│                                                             │
│   ┌─────────────────────────────────────────────────────┐   │
│   │                    CONTROL PLANE                    │   │
│   │  理解：ExecutionPlan、Artifacts、Spec、Reconciler    │   │
│   └──────────────────────────┬──────────────────────────┘   │
│                              │（通用 Lease / Operation）    │
│   ┌──────────────────────────▼──────────────────────────┐   │
│   │                     KERNEL CORE                     │   │
│   │  理解：进程、CPU/GPU Lease、Sandbox                  │   │
│   │  不理解：PyTorch、Token、Checkpoint                 │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

1. **隔离故障域**：Kernel 作为节点 supervisor 和进程管理器运行。若高层训练逻辑或 prompt 模板处理崩溃，Kernel 进程仍必须保持可靠，并能通过 Win32 Job Object 或 POSIX Cgroup 清理子进程树。
2. **缩小信任与特权面**：Kernel 管理硬件 Lease 和 OS 沙箱。将 Product 代码排除在 Kernel 外，可保持安全边界清晰且精简。
3. **通用复用**：同一组 Kernel 基元（`Worker`、`Operation`、`Endpoint`、`Lease`、`Fence`）可服务训练任务（Yield）、模型服务实例（Reactor）和向量检索 worker（Memory Plugin）。
