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
