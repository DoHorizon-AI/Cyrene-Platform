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
2. **Process Tree Cleanup**: AI workers frequently spawn multi-process PyTorch or Triton workers. The Kernel binds workers into OS-level Job Objects / Cgroups so that terminating a worker guarantees zero orphaned GPU memory leaks.
