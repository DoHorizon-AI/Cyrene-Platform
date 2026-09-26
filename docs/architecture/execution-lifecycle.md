# Architecture Deep-Dive: Execution Lifecycle & WorkerControl

This document details the lifecycle management of out-of-process workers and operations.

---

## Why WorkerControl Does Not Expose Raw `argv` / `spawn`

```text
Service / Adapter ──► WorkerControl.start_worker(Spec) ──► Kernel Sandbox Supervisor
                                                                 │
                                                    ┌────────────┴────────────┐
                                                    ▼                         ▼
                                            Win32 Job Object            POSIX Cgroup
```

1. **Resource Fencing**: When a worker starts, the Kernel must atomically attach memory limits, CPU affinities, GPU device node masks, and Lease timers. Exposing unconstrained `spawn(argv)` would bypass sandbox invariants.
2. **Process Tree Cleanup**: Workers may spawn multiple child processes. The
   Kernel binds workers into OS-level Job Objects / cgroups and uses the
   configured lifecycle cleanup mechanisms to bound descendant cleanup. This
   is resource/lifecycle governance, not a guarantee of hostile-code
   containment or zero orphan processes under every failure mode.
---

<!-- Chinese Translation / 中文翻译 -->

# 架构深入解析：执行生命周期与 WorkerControl

本文详细说明进程外 worker 和 operation 的生命周期管理。

---

## 为什么 WorkerControl 不暴露原始 `argv` / `spawn`

```text
Service / Adapter ──► WorkerControl.start_worker(Spec) ──► Kernel Sandbox Supervisor
                                                                 │
                                                    ┌────────────┴────────────┐
                                                    ▼                         ▼
                                            Win32 Job Object            POSIX Cgroup
```

1. **资源 fencing**：worker 启动时，Kernel 必须原子地附加内存限额、CPU affinity、GPU 设备节点掩码和 Lease 计时器。若暴露不受约束的 `spawn(argv)`，就会绕过沙箱不变量。
2. **进程树清理**：worker 可能创建多个子进程。Kernel 会将 worker 绑定到 OS 级 Job Object / cgroup，并使用配置的生命周期清理机制限制后代进程的清理范围。这是资源/生命周期治理，不保证隔离恶意代码，也不保证每种故障模式下绝无孤儿进程。
