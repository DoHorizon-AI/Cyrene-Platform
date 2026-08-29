# ADR-SANDBOX-ADAPTER-BOUNDARY: External Privileged Sandbox Host

- Status: Accepted / Normative
- Date: 2026-08-11

## Decision

`kernel/` is a pure-safe Rust decision core. It owns leases, fencing, launch
authorization, lifecycle state, heartbeat policy, and the generic client for
the local `cyrene.sandbox.v1` UDS protocol. It does not spawn a worker, open a
cgroup file, read `/proc/self/cgroup`, send a signal, call pidfd/BPF/prctl, or
load a container runtime library.

`adapters/execution/sandboxd` is a separately supervised privileged Adapter
Host. The initial backend is native Linux process plus cgroup v2. It owns the
delegated cgroup subtree, pre-flight cleanup of only its proven `instance-*`
children, limits, device-BPF enforcement, process reaping, OOM counters, and
the SIGTERM -> cgroup.kill -> reap cleanup truth. Its Rust may use narrowly
audited Linux `unsafe` where the OS API makes that unavoidable; that exception
does not cross the UDS boundary into Kernel code.

Hardware Adapter Hosts remain independent: they report device facts and a
binding over `cyrene.hardware.v1`; `sandboxd` consumes the Kernel-approved
binding but never selects a vendor or creates a resource lease. A binding that
requests `HARD` must fail closed if device enforcement cannot be installed.

## Docker and runtime backends

Docker/OCI is a future `sandboxd` backend profile, not a Kernel feature and
never a raw Docker CLI input exposed through Core RPC. The selected profile is
host configuration, not a worker-controlled argument. For each worker exactly
one backend owns the cgroup/lifecycle tree:

- native-cgroup: `sandboxd` creates and reaps the cgroup directly;
- oci/docker: the selected runtime owns the container cgroup and lifecycle;
- systemd scope: systemd owns the scope and lifecycle.

Mixing direct cgroup writes with a Docker-owned worker is prohibited. This
avoids both conflicting limits and ambiguous cleanup ownership. The current
implementation ships only `native-cgroup`; OCI/systemd profiles remain an
explicit extension point and must satisfy the same protocol and tests before
being advertised.

## Failure and restart ownership

`cyrene-sandboxd.service` receives cgroup delegation and `KillMode=control-group`;
`cyrene-kernel.service` receives neither. systemd orders Kernel after sandboxd
and makes sandboxd `PartOf` Kernel, so an operator-triggered Kernel restart
also removes the owned worker tree rather than allowing a new Kernel epoch to
adopt it. The worker is also a direct sandboxd child with parent-death cleanup.
The exact service-manager cascade is a mandatory Linux integration test; until
that test exists, operators must treat crash/restart recovery as a P1 gate, not
as a completed HA claim.

## JVM boundary

Java/Kotlin improves in-process reliability (type safety, memory management,
and exception handling), but it is not an OS isolation boundary. A JVM OOM,
native/JNI crash, blocked native call, runaway CPU, or device-node access still
needs cgroup/process/device enforcement. JVM workers therefore run *inside* a
sandboxd-created scope just like Python workers; their language runtime never
replaces the sandbox.

## Acceptance gates

- every Rust source under `kernel/` remains free of `unsafe`, libc, direct
  process spawning, cgroup paths, pidfd, and BPF implementation;
- Kernel cannot become ready unless its configured sandbox UDS identity and
  pre-flight capabilities succeed;
- oversized, malformed, version-mismatched, or identity-mismatched UDS frames
  fail closed;
- HARD device bindings cannot silently become visibility-only;
- service restart, adapter loss, worker hang, OOM, and Docker/OCI ownership
  boundaries are covered by Linux end-to-end tests before production sign-off.
