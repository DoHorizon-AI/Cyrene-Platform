# Platform structure migration audit

This audit compares the original compilable bootstrap with the current
`origin/develop` tree. It records where semantic responsibility moved, rather
than treating a smaller directory as evidence of lost behavior.

本审计比较最初可编译基线与当前 `origin/develop`，追踪语义职责的去向，
不把目录变小本身当作能力丢失证据。机器可读映射见
[`platform-structure-migration-map.json`](platform-structure-migration-map.json)。

## Baselines / 基线

- Original compilable baseline: `4c092fcb7fdb44bc9c0e3e54ecdb9b13bc316b47`
  (`chore: bootstrap CYRENE core`). It is the first repository commit and
  predates the embedded-runtime removal and later decomposition milestones.
- Pre-major-refactor checkpoint: `142280f79fe497301b5f8fe7121f19d2bf922a2f`
  (`refactor(core): remove embedded Python and container tooling`).
- Last verified develop snapshot: `bc3327bdbe1edba6449a0f34d6954b022e4c3dd0`.

## Why Kernel became smaller / Kernel 变小的原因

The history shows intentional boundary extraction, not accidental deletion:

1. `07d723e` and `ef0099c` moved vendor discovery and the Node Agent out of
   `kernel/` into `adapters/` and `agents/`.
2. `f1c76e4` moved privileged sandbox/process implementation to the external
   Sandbox Adapter and retained only a Kernel client plus lifecycle metadata.
3. `a7c0e66` and `dc5f073` split large contract/API/daemon files into domain
   modules; the semantic contract and tests grew while individual files shrank.
4. `30b3d40`, `e69f74f`, and `64d6a32` replaced and then removed the duplicate
   plugin supervisor in favor of the Kernel InstanceActor/watchdog.
5. `142280f` and the subsequent contract milestones removed obsolete embedded
   Python/container and untyped legacy RPC behavior from the open core.

这些提交证明这是边界抽取和模块化，而不是遗漏删除：硬件探测、特权沙箱、
Node Agent、重复 supervisor 被移到其权威层；大型文件被拆成显式领域模块；
旧的嵌入式 Python、容器工具和无类型 RPC 被契约化或归档。

## Migration map summary / 迁移摘要

| Status | Evidence-backed responsibility | Current owner |
| --- | --- | --- |
| STILL_HERE | Kernel authority, lease/fence decisions, worker lifecycle policy, generic protocol validation | `kernel/crates/cy-kernel-api`, `cy-kernel-daemon` |
| MOVED | Node journal/session/upgrade; sandbox client; managed process metadata | `agents/node`, adapters, Kernel daemon |
| SPLIT | Manifest model, semantic contract, Kernel API, daemon implementation | `contracts/rust`, `kernel/crates/cy-kernel-api`, `cy-kernel-daemon` |
| EXTRACTED | Vendor hardware discovery and privileged sandbox execution | `adapters/hardware`, `adapters/execution` |
| REPLACED | Duplicate plugin supervisor and untyped Agent/AI RPC | InstanceActor/watchdog, generic Kernel contracts, and Plugin-owned endpoints |
| DELETED_OBSOLETE | Retired `cy-plugin-supervisor`, legacy `ai_service.proto`/`agent_service.proto` surfaces | Archived migration tooling; no canonical runtime caller |
| LOST_SUSPECTED | None | — |

## Correctness matrix / 正确性矩阵

| Capability | Canonical implementation and evidence |
| --- | --- |
| Principal | `kernel/crates/cy-kernel-daemon/src/peer_cred.rs`; authority rejects calls without transport-derived identity. |
| Worker | `cy-kernel-api/src/authority.rs`, daemon `watchdog/instance_actor.rs`, `rpc/worker_control.rs`; generation and lease are required. |
| Operation | Authority ports plus daemon operation conversion/RPC; lifecycle transitions are contract-tested. |
| Resource | `cy-resource-manager/src/lib.rs`; inventory generation, allocation and lease ownership are fenced. |
| Lease/Fence | `cy-kernel-api/src/lease.rs`, resource manager, daemon release/revoke paths; stale fence and incomplete cleanup tests are present. |
| Capability | `framework/crates/cy-platform-api`; manifest normalization and selection remain generic. |
| Endpoint | Endpoint/grant authority is lease-bound and purged on worker loss/release. |
| Event | Durable event store/history and `WatchEvents` v1/v2 projections. |
| Replay/live handoff | `snapshot` captures the cursor before reading state; ordered replay then notifier handoff is implemented and regression-tested. |
| Cleanup/recovery | Watchdog, service manager and sandbox client require physical cleanup confirmation; restart recovery is epoch/fence scoped. |

The current exact source contains no production Rust matches for the forbidden
product nouns (dataset/training/model-version/evaluation/deployment/LoRA/
tokenizer/product policy). Therefore `KERNEL_CORRECTNESS_CAPABILITIES_LOST=0`
and `BUSINESS_SEMANTICS_REMAINING_IN_KERNEL=0` are evidence-backed outcomes.

当前生产 Rust 源码没有业务禁用词命中，因此正确性能力丢失为 0，Kernel
残留业务语义为 0。完整逐项记录、commit 和路径见 JSON 映射。

## Ownership map / 权责图

- Kernel owns authenticated authority, leases/fences, lifecycle decisions,
  generic protocol validation, and recovery policy.
- Resource Manager owns inventory, allocation, lease state, and release only
  after cleanup confirmation.
- Platform API owns generic capability manifest normalization and selection.
- Package Runtime owns package installation/dependency preparation; it is not a
  second manifest or registry authority.
- Contracts own versioned protocol/schema/manifests.
- Plugins own official package implementations and protocol consumers.
- Product owns bindings, policy, auth/UI, connector semantics and domain data.

Kernel 只拥有通用授权、租约围栏、生命周期决策、协议校验与恢复策略；资源管理器、
Package Runtime、契约、Plugins、Product 各自保留单一权威，未发现第二 registry、
manifest 或 runtime lifecycle owner。Product 业务载荷只进入 Plugin 自有 Endpoint。
