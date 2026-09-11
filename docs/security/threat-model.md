# Platform trust and privilege model

This document describes the current code boundary. It is an architecture and
deployment aid, not a promise that the resource sandbox contains hostile code.

## Components and trust boundaries

| Component | Current role | Boundary and authority |
| --- | --- | --- |
| Kernel daemon | Platform Core authority | Owns principals, leases, fence tokens, lifecycle decisions, and generic events. It does not own vendor discovery or cgroup files. |
| `cyrene-sandboxd` | Privileged Platform Core service | Runs out of process, but remains Core because it creates and cleans owned cgroups, applies device BPF policy, starts/stops workers, and reports runtime evidence. |
| Node agent | Platform agent | Exchanges typed control messages with Kernel; does not become lease authority. |
| Runtime agent | Platform agent | Reports runtime state and progress through the typed protocol; does not become a second lifecycle authority. |
| Provider / hardware Adapter | External extension or reference implementation | Runs behind the versioned hardware UDS protocol and reports resource facts/bindings; its implementation is not linked into the Kernel. |
| Worker / service plugin | External extension | Runs as a Kernel-approved child through sandboxd and exchanges opaque or versioned worker messages. Product behavior remains outside Platform. |

An out-of-process boundary is therefore not automatically a licensing or
trust boundary: sandboxd is an example of a privileged Core service.

## Authentication and transport

* Kernel-to-sandboxd and Kernel-to-hardware-Adapter traffic uses bounded,
  versioned Unix-domain-socket frames. Socket mode/ownership is part of the
  deployment boundary.
* sandboxd and NVIDIA Adapter can require the Kernel's UID/GID using
  `SO_PEERCRED`. The Kernel can require the reverse peer identity for each
  configured endpoint. Linux system Adapter peer flags are currently optional;
  without them, access relies on protected socket permissions.
* Workspace relay connections use TLS certificates, a configured server name,
  and the relay protocol's authenticated hello/session flow.
* Worker payloads are not a Rust trait-object plugin ABI. The Kernel's
  `InstanceActor` transport hook preserves correlation, generation, fence,
  timeout, and cancellation; external workers use the documented wire seam.

## Privilege and enforcement

The sandbox service needs access to its delegated cgroup subtree and, for hard
device policy, the Linux BPF/device-control capability required by the host.
A production deployment must grant sandboxd and any NVIDIA sidecar only the
privileges required by those operations, configure the explicit UDS peer
settings, and keep service sockets inaccessible to untrusted workers.

The implementation currently provides resource accounting/capping, process
ownership and supervision, device visibility/enforcement policy where the
configured backend supports it, pidfd tracking where available, and
fence/lifecycle integrity. These are mechanism guarantees, not a general
arbitrary-code security boundary.

## Non-guarantees and evidence status

Source inspection of the current sandboxd shows cgroup v2 controls,
`cgroup.kill`, pidfd support, parent-death signalling, and cgroup-device BPF.
It does not show user/mount/network namespace setup, seccomp installation, or
capability dropping. Full systemd crash/restart and cross-host mTLS acceptance
remain deployment-level tests; they are not inferred from unit tests or a
successful compilation.
