# API Naming Constitution Migration Plan

**Status: Canonical operation naming is complete; P0-01 through P0-04, P0-06,
and P0-07 are complete. P0-05 remains a separate internal model-identity
decision and is not an API action naming blocker.**

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
It is now enforced in `all_source` mode. The retired API vocabulary is no
longer reachable from the configured Platform source roots. The public API
freeze still requires the separate P0-05 decision if the two Lease
representations are ever exposed as one public model, plus normal
repository-level adoption checks; that open model decision does not make the
canonical action vocabulary incomplete.
---

<!-- Chinese Translation / 中文翻译 -->

# API 命名宪法迁移计划

**状态：规范 action 命名已完成；P0-01 至 P0-04、P0-06 和 P0-07 已完成。P0-05 是单独的内部 model identity 决策，不构成 API action 命名阻塞项。**

本计划记录命名迁移当前的证据。它与宪法分开维护：宪法定义稳定用语，本文件记录临时发现及其解决顺序。

本计划记录命名迁移的当前证据。宪法定义长期稳定的语言，本文件只记录临时盘点结果、优先级和执行顺序；迁移完成后应更新本计划，而不是把临时状态伪装成规范。

## P0 发现项

| ID | 发现 | 证据 | 必要处理 |
|---|---|---|---|
| P0-01 | Authority request name 含有因 symbol collision 引入的 Semantic prefix。 | v1/v2 authority contract 现在使用与 RPC 匹配的直接 wrapper：AcquireLeaseRequest、ReleaseLeaseRequest 和 CancelOperationRequest；legacy KernelService projection 使用明确的 Legacy 名称隔离。 | 已完成：规范 authority 名称与 RPC 一致，不再加 adapter prefix。 |
| P0-02 | Agent/Provider heartbeat 被命名为 worker action。 | v1/v2 authority service 现在暴露 ReportHeartbeat/ReportHeartbeatRequest；plugin lifecycle heartbeat 仍明确命名为 ReportPluginHeartbeat，以体现其区别。 | 已完成：authority fact submission 与 plugin liveness payload 使用不同名称和 owner。 |
| P0-03 | 可 replay 的 stream 被称为 subscription。 | v1/v2 authority service 现在以 WatchEvents/WatchEventsRequest 提供 durable replay 与 live handoff；不会创建 durable subscription resource。 | 已完成：观测使用 Watch；Subscribe 专用于持久化 subscription entity。 |
| P0-04 | 逻辑 plugin lifecycle 与具体 process control 使用 Launch/Terminate。 | 旧 KernelService projection 是唯一仍竞争具体进程词汇的部分；PluginLifecycleService 保留 StartPlugin/StopPlugin 表示逻辑生命周期。 | 已完成：KernelService 现在暴露 LaunchProcess/TerminateProcess；StartPlugin/StopPlugin 继续作为逻辑 lifecycle API。 |
| P0-05 | 存在两个 Rust LeaseState model。 | kernel/crates/cy-kernel-api/src/lease.rs 有 ResourceLease 和 Quarantined；contracts/rust/cy-kernel-contract/src/lease.rs 是 cyrene.semantic.v1.Lease 使用的 semantic LeaseState。旧 Proto projection 还含 ATTACHED。 | 追踪 transition、caller、persistence、replay 和 fixture。保留 semantic LeaseState 为规范类型；仅在证明兼容/resource projection 是不同 model 后，才重命名或退役它。 |
| P0-06 | 规范与镜像 Proto input 同时被跟踪。 | 相同的 contracts/proto/** input 也镜像在 contracts/rust/cy-proto/proto/** 下。 | 已完成：两份 input、descriptor、manifest 和 generated Rust consumer 均已更新并同步。 |
| P0-07 | 旧 resource allocation 词汇仍可访问。 | 旧 direct resource RPC 与 ResourceLeaseManager::reserve 是剩余的 Platform 可达入口。 | 已完成：移除 direct resource command，改用 authority AcquireLease/ReleaseLease；Rust resource port/daemon 改用 acquire。 |

P0 发现项并非同一种问题。P0-01 至 P0-04 是已完成的直接命名迁移；P0-05 是仍有意开放的 model identity 决策；P0-06 是已完成的 contract-source hygiene；P0-07 是已完成的 compatibility surface 与 semantic Rust rename。

## 有序工作包

| 任务 | Owner | 允许修改范围 | 验收条件 |
|---|---|---|---|
| NAMING-P0-01 | Platform contracts | contracts/proto/**、镜像 contracts/rust/cy-proto/proto/**、contract fixture/descriptor | 新的规范 contract 中不得出现因 collision 引入的 request prefix；buf lint、breaking check、mirror check 和 regenerated binding 结果一致。 |
| NAMING-P0-02 | Platform kernel | kernel/**、framework/**、agents/** | Rust call site 使用直接 Proto 词汇；同拼写的无关 API 保持不变；cargo check、test 和 clippy 通过。 |
| NAMING-P0-03 | Platform adapter 与 SDK | adapters/**、runtime/**、sdk/** | 具体执行使用 Launch/Terminate；逻辑生命周期使用 Start/Stop；generated SDK 与 fixture 可编译。 |
| NAMING-P0-04 | Product 与 Plugin consumer | 各仓库的 contract、SDK 和 adapter 路径 | Yield/Reactor/Exchange/Plugins 使用 Platform 术语且不引入本地同义词；仓库 test 覆盖迁移 seam。 |
| NAMING-P0-05 | Governance | tooling/architecture/api-naming.toml、gate 和 CI | 已完成：全源码模式通过，policy 在 all_source mode 执行；不能把跳过或模拟的结果报告为 PASS。 |

每个工作包都应是独立且可审查的变更。不要在一次机械搜索替换 patch 中混入 Proto rename、state-machine merge 和无关 Product extraction。

## 必需执行顺序

宪法与 inventory → 将术语归类为同义词/区分/legacy/重复 → 规范 Proto input → 重新生成 binding 与 descriptor → Rust semantic refactoring → Python/JVM/SDK consumer → adapter protocol 和 state-machine 清理 → 有针对性的 test、相关完整 test、文档 → 全源码 forbidden-vocabulary gate。

当前 Workspace IDE 可用 IDEA/RustRover semantic refactoring 查找和重命名 symbol。Checked-out IDE model 指向规范仓库，因此改动必须在隔离的任务 worktree 中进行；semantic operation 之后仍需在仓库层面验证 generated source、镜像 Proto input、descriptor 和 fixture。

## 当前 gate 状态

迁移 gate 已接入 Platform 的 architecture-governance workflow，并以 all_source mode 强制执行。已淘汰 API 词汇已无法从配置的 Platform source root 访问。Public API freeze 仍要求在未来若要将两种 Lease 表示公开为同一 model 时单独解决 P0-05，并完成常规的 repository adoption check；该未决 model 决策不意味着规范 action 词汇仍不完整。
