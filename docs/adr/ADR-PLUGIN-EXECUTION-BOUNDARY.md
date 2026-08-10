# ADR-PLUGIN-EXECUTION-BOUNDARY: Installable Plugin Isolation

- Status: Approved / Normative
- Date: 2026-08-10
- Supersedes: ADR-PLUGIN-RUNTIME section 2.1A for installable business plugins

## Decision

All installable business plugins run outside the Rust Kernel process. The
supported manifest runtimes are:

- subprocess-python;
- subprocess-jvm;
- service.

Local subprocess plugins use the existing zero-port stdio protocol as the
mandatory baseline. UDS or platform-specific local transports may be added
later without changing the lifecycle contract.

In-process Rust code is permitted only as a statically compiled, audited Core
platform adapter for host, device, transport, or sandbox integration. It is not
an installable plugin runtime and cannot be selected through plugin.toml.

The Core repository must not link PyO3, PyTorch, vLLM, CUDA/ROCm compute
runtimes, or model execution libraries. Python and JVM business code remains in
out-of-process workers owned by the advanced-services or third-party plugin
repository.

## Required invariants

- stdout is reserved for framed protocol messages; logs use stderr or structured
  events;
- plugin launch receives typed, policy-approved inputs and never a shell string;
- plugin identity, protocol version, API version, permissions, and package
  digest are checked before launch;
- worker crash, timeout, cancellation, and protocol corruption become typed
  lifecycle events and cannot crash the Core process;
- restart policy and quarantine are controlled by the Rust supervisor.

## Migration

The historical ADR remains in the repository for context and is marked
Superseded. Existing legacy source is not deleted by this ADR. The P0
manifest change removes the in-process runtime from the installable schema and
Rust manifest model. Core v1 and the supervisor migration are later phases.
