# ADR-001: Kernel Has No Product Semantics

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Initial system sketches placed training hyperparameter checks and serving route lookups inside the Rust kernel runtime. This caused tight coupling and made Kernel crashes cascade to unrelated services.

## Decision
The Kernel core shall remain a pure generic execution substrate. It manages OS processes, memory limits, cgroups/Job Objects, and time-bounded resource leases. It shall contain zero product-specific concepts (no TrainingSpec, no prompt templates, no token counters, no billing).

## Why
1. **Fault Isolation**: Product crashes do not compromise the node-level process supervisor.
2. **Minimal Privileges**: Sandboxing and lease enforcement require elevated permissions; domain logic does not.
3. **Generic Reuse**: Identical primitives serve training, inference, and data indexing.

## Alternatives Considered
- *Monolithic AI Kernel*: Embed PyTorch C++ / vLLM bindings directly in the Kernel. Rejected due to security, stability, and high coupling.

## Consequences
- Product services must compile domain intent into declarative `ExecutionPlan` steps that the Kernel executes via generic `WorkerControl` and `Operation` primitives.
