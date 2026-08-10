# ADR-HARDWARE-ADAPTER-BOUNDARY: External Hardware Adapter Hosts

- Status: Accepted / Normative
- Date: 2026-08-10
- Supersedes: the in-process vendor-adapter exception in earlier architecture notes

## Decision

The CYRENE Kernel is a minimal user-space microkernel. It owns only node-local
resource leases and fencing, cgroup/process isolation, lifecycle/reaping, and
the generic client for local adapter IPC. It does not execute vendor commands,
read vendor-specific sysfs/procfs paths, enumerate vendor device nodes, load a
vendor shared object, or export a vendor C ABI.

Each vendor integration is an independently supervised Adapter Host process.
The initial implementation is `adapters/hardware/nvidia`; AMD and Ascend are
future peers, not branches inside a generic Kernel discovery crate. Kernel and
host use the versioned `cyrene.hardware.v1` Protobuf request/response protocol
over a bounded Unix domain socket frame. The Kernel-side implementation is the
vendor-neutral `cy-adapter-client` crate.

Vendor C ABI is allowed only *inside* an Adapter Host (or a private dynamic
library loaded by it). It is an internal implementation choice, not a CYRENE
public API and not a substitute for process isolation.

## Failure and ownership rules

- Adapter loss yields typed `ADAPTER_UNAVAILABLE` / `DEGRADED` evidence and
  blocks new leases requiring that adapter; it does not by itself kill existing
  workloads.
- Adapter inventory is a versioned, expiring fact. The resource manager keeps
  the lease/fencing authority and rejects stale generations.
- Adapter Hosts must run under their own service manager scope. A Kernel crash
  cannot take an adapter down through shared address space, and an adapter
  crash cannot unwind the Kernel.
- Kernel pre-flight cleanup may remove only cgroups and runtime paths it can
  prove it owns. Prefix-wide deletion and adoption of foreign processes are
  prohibited.
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
- a disconnected adapter cannot produce allocatable inventory or a new lease;
- malformed or oversized UDS frames fail closed without panicking the Kernel;
- adapter protocol compatibility and stale-generation rejection are tested;
- adapter services have independent lifecycle supervision and logs.
