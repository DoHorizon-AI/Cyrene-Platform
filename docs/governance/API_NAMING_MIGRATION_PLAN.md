# API Naming Constitution Migration Plan

**Status: P0-01 through P0-04, P0-06, and P0-07 complete; P0-05 remains a
separate model-identity decision.**

This plan records the current evidence behind the naming migration. It is
deliberately separate from the constitution: the constitution defines the
stable language, while this document records temporary findings and the order
in which they must be resolved.

本计划记录命名迁移的当前证据。宪法定义长期稳定的语言，本文件只记录临时
盘点结果、优先级和执行顺序；迁移完成后应更新本计划，而不是把临时状态伪装
成规范。

## P0 findings

| ID | Finding | Evidence | Required disposition |
| --- | --- | --- | --- |
| P0-01 | Authority request names contained collision-driven `Semantic` prefixes. | The v1/v2 authority contracts now use direct `AcquireLeaseRequest`, `ReleaseLeaseRequest`, and `CancelOperationRequest` wrappers matching their RPCs; legacy KernelService projections are isolated with explicit `Legacy` names. | Complete: canonical authority names match the RPCs without adapter prefixes. |
| P0-02 | Agent/provider heartbeat was named as a worker action. | The v1/v2 authority services now expose `ReportHeartbeat`/`ReportHeartbeatRequest`; plugin lifecycle heartbeat remains explicitly distinct as `ReportPluginHeartbeat`. | Complete: authority fact submission and plugin liveness payloads have separate names and owners. |
| P0-03 | A replayable stream was named as a subscription. | The v1/v2 authority services now expose `WatchEvents`/`WatchEventsRequest` for durable replay plus live handoff; no durable subscription resource is created. | Complete: observation uses `Watch`, while `Subscribe` remains reserved for a persisted subscription entity. |
| P0-04 | Logical plugin lifecycle and concrete process control use `Launch`/`Terminate`. | The old KernelService projection was the only competing concrete-process vocabulary; `PluginLifecycleService` retains `StartPlugin` and `StopPlugin` for logical lifecycle. | Complete: KernelService now exposes `LaunchProcess`/`TerminateProcess`; `StartPlugin`/`StopPlugin` remain the logical lifecycle API. |
| P0-05 | Two Rust `LeaseState` models exist. | `kernel/crates/cy-kernel-api/src/lease.rs` has `ResourceLease` plus `Quarantined`; `contracts/rust/cy-kernel-contract/src/lease.rs` is the semantic `LeaseState` used by `cyrene.semantic.v1.Lease`. The old Proto projection also has `ATTACHED`. | Trace transitions, callers, persistence, replay, and fixtures. Keep semantic `LeaseState` canonical; rename or retire the compatibility/resource projection only after proving it is a distinct model. |
| P0-06 | Canonical and mirrored Proto inputs are both checked in. | The same `contracts/proto/**` inputs are mirrored under `contracts/rust/cy-proto/proto/**`. | Complete: both inputs, descriptors, manifests, and generated Rust consumers were updated and synchronized. |
| P0-07 | Legacy resource allocation vocabulary is still reachable. | The old direct resource RPCs and `ResourceLeaseManager::reserve` were the remaining Platform reachability points. | Complete: direct resource commands were removed in favor of authority `AcquireLease`/`ReleaseLease`, and the Rust resource port/daemon use `acquire`. |

The P0 findings are not all the same kind of problem. P0-01 through P0-04
contain completed direct naming migrations; P0-05 is a model-identity decision
that remains intentionally open; P0-06 is completed contract-source hygiene;
and P0-07 is a completed compatibility-surface and semantic Rust rename.

## Ordered work packages

| Task | Owner | Allowed write scope | Acceptance condition |
| --- | --- | --- | --- |
| `NAMING-P0-01` | Platform contracts | `contracts/proto/**`, mirrored `contracts/rust/cy-proto/proto/**`, contract fixtures/descriptors | No collision-driven request prefixes in new canonical contracts; `buf lint`, breaking check, mirror check, and regenerated bindings agree. |
| `NAMING-P0-02` | Platform kernel | `kernel/**`, `framework/**`, `agents/**` | Rust call sites use the direct Proto vocabulary; unrelated same-spelling APIs remain unchanged; `cargo check`, tests, and clippy are clean. |
| `NAMING-P0-03` | Platform adapters and SDK | `adapters/**`, `runtime/**`, `sdk/**` | Concrete execution uses `Launch`/`Terminate`; logical lifecycle uses `Start`/`Stop`; generated SDKs and fixtures compile. |
| `NAMING-P0-04` | Product and Plugin consumers | Each repository's contract, SDK, and adapter paths | Yield/Reactor/Exchange/Plugins consume Platform terms without local synonyms; repository tests cover the migrated seam. |
| `NAMING-P0-05` | Governance | `tooling/architecture/api-naming.toml`, gate and CI | Complete: full-source mode is green and the policy is enforced in `all_source` mode; no skipped or simulated result is reported as PASS. |

Each work package is a separate reviewable change. Do not mix a Proto rename,
a state-machine merge, and an unrelated Product extraction in one mechanical
search/replace patch.

## Required execution sequence

```text
constitution and inventory
        -> classify synonym / distinction / legacy / duplicate
        -> canonical Proto inputs
        -> regenerate bindings and descriptors
        -> Rust semantic refactoring
        -> Python / JVM / SDK consumers
        -> adapter protocol and state-machine cleanup
        -> focused tests, full relevant tests, docs
        -> all-source forbidden-vocabulary gate
```

IDEA/RustRover semantic refactoring is available in the current Workspace IDE
for symbol discovery and rename. The checked-out IDE model points at the
canonical repositories, so changes must be made in the isolated task worktree;
generated sources, mirrored Proto inputs, descriptors, and fixtures still need
repository-level verification after the semantic operation.

## Current gate status

The migration gate is wired into Platform's architecture-governance workflow.
It is now enforced in `all_source` mode. The public API freeze still depends
on the separate P0-05 lease-model decision and repository-level adoption
checks, but the retired API vocabulary itself is no longer reachable from the
configured Platform source roots.
