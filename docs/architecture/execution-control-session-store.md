# Execution control session persistence

`cy-execution-control` exposes an optional `ExecutionSessionStore`, a private-file implementation `FileExecutionSessionStore`, and `ExecutionControlService::with_session_store`. Install the store before cloning or serving the service. The existing constructor remains in-memory for compatibility; adding this library does not enable persistence in a deployment automatically.

The snapshot preserves Host resume credentials, Runtime enrollment grants and resume credentials, pending proof digests, Runtime-to-Node bindings, highest admitted Node epochs, acknowledged assignment correlations, and immutable classified Runtime terminal observations. Terminal observations carry the original `assignment_id` and Product `attempt_id`; the service writes them before acknowledging the Agent frame or publishing the in-memory observation. It deliberately excludes live streams, readiness and pending response channels. Host and Runtime agents must authenticate and reconnect after a control restart. Expired grants remain subject to enrollment validation.

The snapshot is control correlation and execution-evidence state. It does not authorize resource use, derive Product success, publish output Artifacts, or replace Kernel Lease/Fence verification. The existing durable execution intent ledger remains separate. `ExecutionController::terminal_observation` and `wait_terminal_observation` let a Product supervisor read the durable fact without treating the broadcast channel as a queue. Runtime Agent process adoption is not provided by this change.

`ExecutionController::stop` validates the durable completed dispatch, routes a correlated `StopCommand` only to the Runtime generation that accepted the assignment, and waits for its `StopAck`. Stop routing uses the accepted Runtime binding and remains available while the Host Agent control session is temporarily offline. The returned `Accepted` disposition confirms command acceptance, not process termination. The supervisor must then await durable terminal evidence and only release the canonical Lease after applying Product success/failure and Artifact publication rules. If terminal evidence wins the race or a stop is repeated after completion, the controller returns `AlreadyTerminal` with that immutable evidence. A timeout without terminal evidence remains an unknown outcome requiring reconciliation.

`FileExecutionSessionStore::open(directory)` requires a real, private directory (0700 on Unix), holds an exclusive owner lock, and reads only a regular private snapshot (0600) under 64 MiB. Saving writes a new exclusive temporary file, syncs its content, atomically replaces the snapshot, and syncs the parent directory on Unix. Failures return a redacted reconciliation error; invalid snapshots are retained and prevent initialization. Configure a persistent volume and protect backups as credentials. This file store is for one control owner, not multiple replicas with independent volumes.

The deployment assembly can attach it to a newly constructed service:

```rust,ignore
use std::sync::Arc;
use cy_execution_control::FileExecutionSessionStore;

let service = service.with_session_store(Arc::new(
    FileExecutionSessionStore::open(session_directory)?,
))?;
```

Linux validation on 2026-09-26 uses the repository-pinned Rust 1.96.1 toolchain. Tests cover exclusive ownership, disk reopen, credential/epoch/binding/terminal round trips, immutable duplicate terminal replay, durable terminal lookup, authenticated stop acknowledgement, Host-offline Runtime stop routing, graceful process-group termination, and the Kernel UDS/mTLS path. A full production control restart with two GPU workers has not been accepted.
