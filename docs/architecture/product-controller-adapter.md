# Architecture Deep-Dive: Product, Controller & Adapter

This document explains why high-level Product state is strictly separated from low-level execution primitives.

---

## Why `TrainingRun` Is Not a Kernel `Operation`

| Concept | Layer | Lifecycle & Scope | Failure Recovery |
|---|---|---|---|
| **`TrainingRun`** | Product (Yield) | Spans hours/days, contains multiple phases (setup, fine-tune, eval, export) | Re-planned by Reconciler upon step failure |
| **`Operation`** | Kernel (Platform) | A single discrete, time-bounded execution attempt inside a sandbox | Terminated on lease expiry; produces exit code |

A `TrainingRun` is a business entity with user-visible history, billing records, and domain metrics. A Kernel `Operation` is an ephemeral execution sandbox. Conflating them would tie the Kernel to domain-specific persistence schemas.
