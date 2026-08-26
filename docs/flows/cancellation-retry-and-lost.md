# End-to-End Flow: Cancellation, Retry, and Lost Handling

This document details how the Control Plane handles lease timeouts, worker crashes, and attempt fencing.

---

## Failure Recovery & Fencing Model

```text
┌─────────────────────────────────────────────────────────────┐
│                     RECONCILER TIMELINE                     │
├─────────────────────────────────────────────────────────────┤
│  Attempt 1 (Generation 1) starts with Lease TTL = 300s      │
│  ... Worker becomes unresponsive (heartbeat missed) ...     │
│  Lease expires ──► Kernel fences Attempt 1                  │
│  Reconciler marks Attempt 1 as LOST                         │
│  Reconciler spawns Attempt 2 with Generation 2              │
│  If Attempt 1 wakes up late: Kernel rejects stale I/O       │
│  because Lease Token & Generation 1 are invalid!            │
└─────────────────────────────────────────────────────────────┘
```

1. **Monotonic Generation**: Every retry increments the `Generation` counter. Late responses from zombie workers are discarded.
2. **Lease Expiry Fencing**: If a worker loses its lease, the Kernel revokes device access and terminates the process tree.
