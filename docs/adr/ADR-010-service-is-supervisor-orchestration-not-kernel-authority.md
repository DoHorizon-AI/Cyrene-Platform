# ADR-010: Service is a Supervisor Orchestration Abstraction, Not a Kernel Semantic Authority Entity

- **Status**: ACCEPTED
- **Date**: 2026-08-27

## Context
Hosting long-running workloads (e.g. model workers, HTTP/gRPC services) requires generic process management mechanisms: working directory configuration, multi-strategy readiness probing (`ProcessAlive`, `TcpSocket`, `HttpGet`, `WorkerControl`), deterministic exponential restart backoff, and endpoint publication synchronization.

In introducing `ServiceSpec`, `ServiceState`, `ServiceStatus`, `ServiceEvent`, and `ServiceSupervisor`, there is a risk of future architectural erosion where developers might observe these names and attempt to promote `Service` into a 10th authoritative Kernel semantic entity by introducing `ServiceId`, `ServiceRepository`, `ServiceLedger`, `ServiceLease`, or persistent service tables.

## Decision
1. **Semantic Model Frozen**: The Kernel semantic model in `cy-kernel-contract` is strictly frozen to the canonical nouns: `Worker`, `Operation`, `Endpoint`, `EndpointGrant`, `Lease`, `Event`, `Principal`, `Provider`, and `Capability`.
2. **Service is Orchestration Only**: `Service` (`ServiceSpec`, `ServiceSupervisor`) is strictly a **daemon-level generic process supervision abstraction**, NOT an authoritative Kernel domain entity.
3. **No Kernel Authority Extensions**:
   - **NO** authoritative `ServiceId` resource in `cy-kernel-contract`.
   - **NO** `ServiceRepository`, `ServiceLedger`, or persistent `Service` tables in Kernel authority.
   - **NO** independent authoritative `Service` event stream in the gRPC/UDS authority ledger.
4. **Primitive Composition**: `ServiceSupervisor` drives existing Kernel primitives: `LaunchPlan`, `ProcessRuntime` / `SandboxBackend`, `CleanupReport`, and `semantic::Endpoint`.

## Why
1. **Prevents Conceptual Bloat**: Avoids duplicating the `Worker` and `Operation` execution lifecycle with redundant "Service" ledger persistence.
2. **Clear Boundary**: The Kernel authority continues to govern identity, leases, fence tokens, and physical capabilities, while the supervisor manages process lifecycle loops, readiness probing, and backoff timing.
3. **Future Drift Prevention**: Establishes an explicit boundary so future contributors do not build parallel ledger state for long-running processes.

## Consequences
- Long-running application workloads are represented to the supervisor via `ServiceSpec` and executed over standard `ProcessRuntime` and `LaunchPlan`.
- Service health and readiness translate directly into standard `semantic::Endpoint` publication and revocation.
