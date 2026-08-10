# ADR-HARDWARE-ADAPTER-BOUNDARY: External Hardware Adapter Hosts

- Status: Accepted / Normative
- Date: 2026-08-10
- Supersedes: the in-process vendor-adapter exception in earlier architecture notes

## Decision

The CYRENE Kernel is a minimal user-space microkernel. It owns only node-local
resource leases and fencing, lifecycle decisions, and generic clients for
local adapter IPC. cgroup/process isolation and reaping belong to the external
Sandbox Adapter Host. The Kernel does not execute vendor commands,
read vendor-specific sysfs/procfs paths, enumerate vendor device nodes, load a
vendor shared object, or export a vendor C ABI.

Each vendor integration is an independently supervised Adapter Host process.
`adapters/hardware/nvidia` is the current deployment example; AMD, Ascend,
Intel, virtual partitioners, and private accelerator integrations are peers,
not branches inside a generic Kernel discovery crate. Kernel and host use the
versioned `cyrene.hardware.v1` Protobuf request/response protocol over a
bounded Unix domain socket frame. The Kernel-side implementation is the
vendor-neutral `cy-adapter-client` crate.

The Kernel is configured with one or more explicit `adapter_id=absolute-uds`
endpoints. It does not infer a vendor from the adapter ID, auto-discover a
sidecar, or contain a vendor fallback. The adapter client verifies the response
identity, records the configured adapter ID as every device's provenance,
creates a Kernel-local aggregate inventory generation, and routes a device
binding only back to its provenance adapter. Duplicate physical device IDs
across sidecars fail closed rather than selecting an arbitrary owner.

Vendor C ABI is allowed only *inside* an Adapter Host (or a private dynamic
library loaded by it). It is an internal implementation choice, not a CYRENE
public API and not a substitute for process isolation.

## Failure and ownership rules

- Adapter loss yields typed `ADAPTER_UNAVAILABLE` / `DEGRADED` evidence and
  blocks new leases requiring that adapter; it does not by itself kill existing
  workloads.
- Adapter inventory is a versioned, expiring fact. The resource manager keeps
  the lease/fencing authority and rejects stale generations. The registry
  aggregates independently versioned sidecar facts into its own monotonic
  Kernel generation; it never compares vendor-local generations directly.
- Adapter Hosts must run under their own service manager scope. A Kernel crash
  cannot take an adapter down through shared address space, and an adapter
  crash cannot unwind the Kernel.
- Sandbox Adapter Host pre-flight cleanup may remove only cgroups and runtime
  paths it can prove it owns. Prefix-wide deletion and adoption of foreign
  processes are prohibited.
- Worker health requires IPC heartbeats plus deadline policy; PID liveness,
  cgroup telemetry, and a successful old inventory sample are insufficient.

## Consequences

This adds one local IPC hop and a small protocol mapping layer. In return,
driver failures, blocking vendor calls, and FFI memory corruption stop at the
Adapter Host boundary. The Kernel remains language-neutral at its externally
visible boundary: Kotlin, Python, Rust, C/C++, and future services consume
versioned contracts rather than a Rust or vendor-specific ABI.

## Acceptance gates

- no vendor command, vendor shared-library loading, or vendor device-path scan
  exists under `kernel/`;
- Kernel startup accepts multiple explicit adapter endpoints and has no
  vendor-specific endpoint, service dependency, or fallback path;
- a device binding is routed only to the adapter that supplied its provenance,
  and duplicate device IDs across adapters fail closed;
- a disconnected adapter cannot produce allocatable inventory or a new lease;
- malformed or oversized UDS frames fail closed without panicking the Kernel;
- adapter protocol compatibility and stale-generation rejection are tested;
- adapter services have independent lifecycle supervision and logs.
