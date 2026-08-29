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
