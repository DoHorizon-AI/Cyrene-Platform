# CYRENE Kernel Execution Goals

**Status:** active implementation and acceptance tracker. This document does
not redefine semantic v1; the authoritative v1 contract is
[kernel-semantic-contract-v1.md](../contracts/kernel-semantic-contract-v1.md).

The design intent is recorded in
[Kernel Design Goals](../architecture/kernel-design-goals.md). The deployment
shape and operator-facing baseline are in
[Kernel Runtime Baseline](kernel-runtime.md).

## Current baseline

The following work is already represented in the repository and is not a new
execution phase:

- Kernel daemon responsibilities have been split into a composition root and
  focused adapter, conversion, and RPC modules; do not reopen a structural
  split just because of the former monolithic document.
- The v1 semantic contract and its TCK live under `contracts/`; v1 uses the
  contract's bounded cursor-to-page event projection.
- The Linux-oriented runtime boundary is composed from the pure Safe Kernel,
  an external sandbox service, and external hardware adapters. Its operational
  assumptions are documented in `kernel-runtime.md`.

## P2 implementation and acceptance goals

### 1. Local privileged-IPC admission

Make the admission boundary executable, not only documented.

- Configure and enforce an explicit peer policy for Kernel-to-sandbox and
  Kernel-to-hardware-adapter UDS connections (dedicated account or group).
- Require peer-credential validation for every privileged request path.
- Demonstrate that an untrusted local account cannot issue a raw privileged
  UDS request, while the authorized Kernel identity can.
- Keep socket ownership, mode, service-account, and expected-peer settings
  coherent across systemd units and adapter configuration.

### 2. Linux process and resource isolation evidence

Validate the actual Linux environment, rather than inferring isolation from a
Windows build or from the presence of adapter code.

- Verify cgroup v2 hierarchy and the controllers required by the selected
  resource policy.
- Exercise create, attach, terminate, and cleanup paths, including
  `cgroup.kill` where supported, pidfd signaling, reaping, timeout, and
  out-of-memory outcomes.
- Prove device and GPU restrictions in the chosen isolation backend, including
  a denial case for an unleased or unauthorized device.
- Capture the kernel, cgroup, service-manager, and adapter versions with each
  acceptance run so results are reproducible.

### 3. Authority, lifecycle, and recovery behaviour

- Run concurrent lease, renewal, release, and stale-fence tests against real
  provider state; a stale authority must never mutate a newer lease.
- Validate worker start, stop, unexpected exit, restart, and reconciliation.
  Recovery must clean up or reconcile known ownership and must not adopt an
  unverified foreign process.
- Exercise provider inventory drift and confirm reconciliation yields an
  explainable result and appropriate semantic events.

### 4. Contract and integration conformance

- Run the semantic v1 TCK and the applicable Rust, gRPC, runtime, sandbox, and
  hardware-adapter tests for each change.
- Verify event cursor, ordering, retention-expiry, and slow-consumer behaviour
  according to the v1 bounded-page contract.
- Treat semantic-contract changes as versioned changes: update the contract,
  generated projections, and TCK together before integration.

## Evidence required to close a goal

A goal is complete only when its relevant automated tests pass and the target
Linux acceptance environment has produced retained evidence for the stated
security or isolation claim. Local compilation is useful feedback but is not
evidence of Linux cgroup, pidfd, BPF/device, GPU, or peer-credential behaviour.

Report each acceptance run with the commit, environment identity, commands or
test suite, expected result, actual result, and any unsupported platform
feature. Unsupported capabilities must be surfaced explicitly rather than
being represented as a successful no-op.

## Deferred semantic evolution

Namespace-scoped identity, snapshot-plus-stream events, server-streaming
delivery, new provider operations, C ABI evolution, and JVM/Kotlin projections
remain design candidates. They are not P2 acceptance criteria until a new or
revised semantic contract and its conformance cases are approved.
