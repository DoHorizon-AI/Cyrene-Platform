# Cyrene API Naming Constitution

**Status: Normative for new and migrated APIs before the first public release.**

This document is the canonical naming vocabulary for Cyrene. Platform owns the
generic vocabulary; Yield, Reactor, Exchange, Catalyst, Echo, Navigator, and
Plugins consume it and may add domain terms only when the domain distinction is
real and documented.

本文件是 Cyrene 的统一命名宪法。Platform 负责通用词汇；Yield、Reactor、
Exchange、Catalyst、Echo、Navigator 与 Plugins 必须复用这些词，只有在确有
领域差异并写明差异时才能增加领域专用名词。

## 1. Constitution

The rules are semantic, not cosmetic:

1. One preferred term per concept.
2. One meaning per term.
3. A name must identify its abstraction layer: logical lifecycle, concrete
   execution, resource ownership, observation, or control-plane policy.
4. A breaking rename is allowed before the first public API freeze. Compatibility
   names must be explicitly marked and isolated; they are not a second authority.
5. The first public release starts the naming freeze:
   **Cyrene public API naming freeze begins with the first public release.**

这些规则是语义规则，不是审美偏好：一个概念只有一个首选词，一个词只能有
一个含义；命名必须明确自己属于逻辑生命周期、具体执行、资源所有权、观测或
控制面策略层。首个 public release 之前允许 breaking rename；兼容名称必须
明确隔离，不能发展成第二套权威。

## 2. Canonical action vocabulary

| Concept | Canonical | Precise meaning | Use | Avoid for this meaning |
| --- | --- | --- | --- | --- |
| Lease acquisition | `Acquire` | Obtain a resource with ownership/lease semantics. | `AcquireLease`, `acquire_lease` | allocation aliases or claim-style verbs |
| Lease extension | `Renew` | Extend an existing lease's validity. | `RenewLease`, `renew_lease` | `refresh_lease`, `extend_lease` |
| Owner release | `Release` | The holder actively relinquishes a lease. | `ReleaseLease`, `release_lease` | `free_resources`, `return_lease` |
| Forced control-plane reclaim | `Revoke` | The authority cancels a still-valid lease or capability. | `RevokeLease`, `REVOKED` | `Release` for forced reclaim |
| Logical object creation | `Create` | Create an identified entity with a lifecycle. | `CreateOperation`, `CreateWorker` | Random mixing of `new`, `make`, `provision` |
| Logical object deletion | `Delete` | Delete a persistent/logical object. | `DeleteWorkspace` | Mixing `remove` and `delete` |
| Logical entity start | `Start` | Move a managed entity into its running lifecycle. | `StartWorker`, `StartService`, `StartPlugin` | process-launch verbs for logical objects |
| Concrete execution launch | `Launch` | Create a process, execution, or runtime instance. | `LaunchProcess`, `ProcessRuntime::launch` | `Start` for fork/exec |
| Logical entity stop | `Stop` | Request an orderly stop of a managed entity. | `StopWorker`, `StopService`, `StopPlugin` | process-termination verbs for logical objects |
| Concrete execution termination | `Terminate` | End a concrete process or execution. | `TerminateProcess`, `terminate()` | Mixing `StopProcess` and `TerminateProcess` |
| Final resource cleanup | `Cleanup` | Remove residual resources after execution ends. | `cleanup_execution` | Random `delete`, `remove`, `purge` |
| Single-object query | `Get` | Read a known object or snapshot by stable identity. | `GetWorker`, `GetLease` | Mixing `Fetch`, `Read`, `Query` |
| Finite history query | `Read` | Read a bounded durable history page after a cursor. | `ReadEvents` | subscription or fetch-style aliases |
| Collection enumeration | `List` | Return a collection of objects. | `ListWorkers` | `GetWorkers`, `EnumerateWorkers` |
| Long-lived observation | `Watch` | Observe changes through a server stream or long-lived connection. | `WatchEvents`, `WatchOperations` | subscription verbs unless a subscription is created |
| Durable subscription | `Subscribe` | Create a cursor/delivery-semantic subscription. | `Subscribe` when such a resource exists | Calling ordinary watch a subscription |
| Agent/provider fact submission | `Report` | Submit an observed fact to its authority. | `ReportHeartbeat`, `ReportOperation` | heartbeat-named authority reports |
| Identity registration | `Register` | Add an existing identity to the control plane. | `RegisterWorker`, `RegisterProvider` | `Create` for registration |
| Incomplete operation cancellation | `Cancel` | Ask an operation not to continue. | `CancelOperation` | Random `StopOperation`, `AbortOperation` |
| Forced internal interruption | `Abort` | Immediately interrupt an unsafe/error-path execution. | Internal `abort_execution` | Ordinary user cancellation |
| Execution preparation | `Prepare` | Prepare work before launch. | `PrepareExecution` | Mixing `Initialize`, `Setup` |
| Health probe/read | `Check` / `GetHealth` | Actively check or read health. | `health_check()`, `GetHealth` | `Ping` as a health contract |
| External fact refresh | `Refresh` | Re-read facts from the external world. | `RefreshInventory` | Lease TTL refresh |

`Start` and `Launch` are deliberately different: `Start` changes the lifecycle
of a managed logical entity; `Launch` creates the concrete runtime instance.
Likewise `Stop` and `Terminate` are different, and `Cleanup` is the post-
termination resource phase.

## 3. Canonical concept vocabulary

| Concept | Canonical meaning | Do not substitute |
| --- | --- | --- |
| `State` | The current position in one lifecycle state machine. | `Status`, `Phase` |
| `Status` | A current aggregate result composed from multiple facts. | `State` |
| `Phase` | An internal step inside one operation. | `State` |
| `Event` | An immutable record of an occurred fact or state change. | `Notification` |
| `Fact` | An objective observation of the external world. | `State` |
| `Spec` | A declarative description of the desired object or execution. | `Config` |
| `Config` | Configuration for a daemon, client, or runtime component itself. | `Spec` |
| `Policy` | Governance constraints on what is allowed, denied, or limited. | `Config` |
| `Options` | Non-core optional modifiers of one invocation. | `Config` |
| `Metadata` | Descriptive labels and annotations. | `Spec` |
| `Id` | A persistent logical identity. | `Key`, `Handle` |
| `Key` | A lookup key for a map or database. | `Id` |
| `Handle` | A control reference to a live runtime object. | `Id` |
| `Ref` | A reference locating another object. | `Handle` |
| `Token` | A fencing, authentication, or capability token. | `Id` |
| `Error` | An API call failure. | `Reason` |
| `Reason` | Why the current state or status was entered. | `Error` |
| `Cause` | The underlying cause that formed an error or reason. | `Reason` |

For example, `WorkerState`, `ServiceState`, and `OperationPhase` can all be
correct because they name different state-machine layers. A `WorkerStatus`
must not be introduced when it merely duplicates the Worker's lifecycle state.

## 4. Platform entity vocabulary

| Entity | Exactly one meaning |
| --- | --- |
| `Principal` | A subject with identity and authorization semantics. |
| `Provider` | An implementation that supplies a resource or capability. |
| `Resource` | A discoverable capability/entity that can be assigned and leased. |
| `Lease` | Time-bounded control over a `Resource`. |
| `Worker` | A work-executing subject managed by Platform. |
| `Operation` | A lifecycle-bearing, observable, cancellable work request. |
| `Capability` | A capability a subject or Provider can provide or execute. |
| `Endpoint` | An address at which a running service is reachable. |
| `Event` | An immutable change record. |
| `Binding` | The concrete association between a resource and an execution/consumer. |
| `Execution` | One concrete runtime instance. |
| `Process` | An OS-level process. |
| `Service` | A long-lived managed service. |

The following distinctions are mandatory:

```text
Worker    != Process
Operation != Execution
Resource  != Device
Lease     != Allocation
```

An adapter may expose a `Device` as one kind of `Resource`, but the two words do
not become interchangeable. An `Allocation` is a result or binding detail; a
`Lease` is the time-bounded authority that governs it.

## 5. Protobuf and RPC rules

### 5.1 Names

Public RPCs use `VerbNoun`; their request and response wrappers use the exact
RPC name:

```proto
rpc AcquireLease(AcquireLeaseRequest)
    returns (AcquireLeaseResponse);

rpc ReportHeartbeat(ReportHeartbeatRequest)
    returns (ReportHeartbeatResponse);

rpc ReadEvents(ReadEventsRequest)
    returns (ReadEventsResponse);

rpc WatchEvents(WatchEventsRequest)
    returns (stream WatchEventsResponse);
```

Do not add `Semantic`, `Core`, `Sandbox`, `Hardware`, or another adapter prefix
to a request merely to work around a symbol collision. Resolve the collision by
choosing the owning package, extracting a shared request, or explicitly naming
the compatibility projection. Collision-prefixed semantic request names must not be
extended or copied into new contracts; the machine-readable forbidden-name list
is the enforcement source.

### 5.2 Package owns the domain

The package owns the domain and the type names describe the concepts:

```text
cyrene.sandbox.v1.LaunchRequest
cyrene.sandbox.v1.ExecutionLimits
cyrene.sandbox.v1.ProcessHandle
```

Avoid redundant forms such as `SandboxLaunchRequest`, `SandboxLimits`, and
`SandboxProcessHandle` inside `cyrene.sandbox.v1`. The package is already the
domain boundary.

### 5.3 Wire evolution

Renaming a Protobuf message or RPC is a breaking contract change. Before the
first public release, migrate all checked-in contract inputs, generated
bindings, fixtures, descriptors, adapters, and consumers in one coordinated
change. After publication, use a versioned contract or an explicitly isolated
compatibility package; never silently reassign a wire symbol or field number.

## 6. Rust and SDK rules

Cross-language APIs are mapped directly, not translated through synonyms:

```text
Proto              Rust
AcquireLease    -> acquire_lease()
RenewLease      -> renew_lease()
ReleaseLease    -> release_lease()
StartWorker     -> start_worker()
StopWorker      -> stop_worker()
ReportHeartbeat -> report_heartbeat()
ReadEvents      -> read_events()
WatchEvents     -> watch_events()
```

Resource-manager allocation aliases are not acceptable Rust spellings for the
canonical lease operation. The semantic rename must distinguish it from
unrelated methods such as `Vec::reserve`; use IDEA/RustRover symbol refactoring
or compiler-guided edits, never a blind repository-wide replacement.

## 7. State-machine rule

**One canonical entity has one canonical lifecycle state machine.**

Finding two types with the same name is not a rename-only task. Before merging
or deleting a state type, trace:

1. every state and transition;
2. every caller and adapter boundary;
3. persistence, replay, and serialization/deserialization;
4. terminal-state and recovery behavior;
5. tests and fixtures that assert the state machine.

If the models are identical, merge them into the canonical type. If they are
different, rename the non-canonical model to expose its domain or compatibility
boundary. For the current Platform inventory, semantic
`cyrene.semantic.v1.LeaseState` is the canonical lease lifecycle; the older
`cyrene.core.v1.ResourceLease` projection has a different payload and an
`ATTACHED` compatibility state, so it must be treated as a separate migration
surface until its callers and persistence are retired. It must not be silently
merged with the semantic type.

## 8. Refactoring protocol

The required order for a breaking migration is:

1. Read this constitution and create the canonical vocabulary table.
2. Inventory public and internal names across Platform, Products, and Plugins.
3. Classify each hit as a true synonym, intentional distinction, legacy
   compatibility, or duplicate model.
4. Use IDEA/RustRover `Rename Symbol`, `Find Usages`, `Safe Delete`, `Change
   Signature`, and `Move Symbol` when available.
5. Normalize canonical Protobuf inputs first; regenerate every binding and
   descriptor.
6. Run semantic refactoring for Rust and then migrate Python, JVM, and SDK
   consumers.
7. Clean adapter protocols, state-machine ownership, tests, fixtures, and docs.
8. Run compiler/build checks, focused tests, full relevant suites, and the
   forbidden-vocabulary gate.

The current IDE integration exposes semantic symbol lookup and rename actions.
Generated bindings and descriptor/fixture references still require repository-
wide contract search and regeneration; an IDE result alone is not proof of
wire-contract completeness.

## 9. Forbidden vocabulary gate

The machine-readable policy is
[`tooling/architecture/api-naming.toml`](../../tooling/architecture/api-naming.toml)
and the checker is
[`tooling/ci/check-api-naming.py`](../../tooling/ci/check-api-naming.py).

CI now runs in `all_source` mode for the configured source roots and rejects
every occurrence of the retired vocabulary. Documentation, changelogs,
migration notes, and explicitly retained compatibility fixtures are excluded.
A skipped or unrun check is not a pass.

The complete forbidden symbol list is maintained only in
`tooling/architecture/api-naming.toml` as a machine-readable regression guard.
Current APIs, comments, examples, and governance prose must refer to the
canonical vocabulary rather than reproducing retired spellings. Generic words
such as `reserve` or `start` are not rejected by text matching because
unrelated APIs (for example `Vec::reserve`) must remain valid; the Platform
resource port uses the semantic `acquire` spelling.

## 10. Adoption by Product and Plugin repositories

Yield, Reactor, Exchange, Catalyst, Echo, Navigator, and Plugins must:

- link to this constitution from their README or developer guide;
- use Platform terms at every shared seam and in cross-repository examples;
- keep Product-owned state, policy, specs, and metadata in the Product while
  retaining the canonical names for those concepts;
- not introduce local aliases for `Acquire`/`Reserve`, `Start`/`Launch`,
  `Stop`/`Terminate`, or `Watch`/`Subscribe`;
- classify any deliberate domain exception in the repository's own contract
  document and add a focused regression test.

Platform remains the language source. A future shared vocabulary package or
TCK may distribute this policy, but it must reference this document rather than
redefine the terms independently.
