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
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 结构迁移审计

本审计比较最初可编译 bootstrap 与当前 \`origin/develop\) 树，记录语义责任迁移到哪里，而不是把目录变小当作行为丢失的证据。

本审计比较最初可编译基线与当前 \`origin/develop\)，追踪语义职责的去向，不把目录变小本身当作能力丢失证据。机器可读映射见 [\`platform-structure-migration-map.json\`](platform-structure-migration-map.json)。

## 基线

- 最初可编译基线：\`4c092fcb7fdb44bc9c0e3e54ecdb9b13bc316b47\)（\`chore: bootstrap CYRENE core\`）。这是仓库首个 commit，早于 embedded-runtime 移除及后续分解里程碑。
- 大规模重构前检查点：\`142280f79fe497301b5f8fe7121f19d2bf922a2f\)（\`refactor(core): remove embedded Python and container tooling\`）。
- 最近一次已验证的 develop 快照：\`bc3327bdbe1edba6449a0f34d6954b022e4c3dd0\)。

## Kernel 变小的原因

历史显示这是有意的边界抽取，不是意外删除：

1. \`07d723e\` 和 \`ef0099c\` 将厂商 discovery 和 Node Agent 从 \`kernel/\` 移到 \`adapters/\` 与 \`agents/\`。
2. \`f1c76e4\` 将特权 sandbox/process 实现移至外部 Sandbox Adapter，Kernel 仅保留 client 和生命周期 metadata。
3. \`a7c0e66\` 和 \`dc5f073\` 将大型契约/API/daemon 文件拆分为领域 module；semantic contract 和测试增加，而单个文件缩小。
4. \`30b3d40\`、\`e69f74f\) 和 \`64d6a32\` 先替换、再移除重复 plugin supervisor，改由 Kernel InstanceActor/watchdog 负责。
5. \`142280f\` 和后续契约里程碑从 open core 移除过期的 embedded Python/container 和无类型 legacy RPC 行为。

这些提交证明这是边界抽取和模块化，而不是遗漏删除：硬件探测、特权沙箱、Node Agent 和重复 supervisor 均移至其权威层；大型文件拆成显式领域 module；旧的 embedded Python、容器工具和无类型 RPC 被契约化或归档。

## 迁移映射摘要

| 状态 | 有证据支持的职责 | 当前 owner |
|---|---|---|
| STILL_HERE | Kernel authority、Lease/Fence 决策、Worker 生命周期策略、通用协议校验 | \`kernel/crates/cy-kernel-api\`、\`cy-kernel-daemon\` |
| MOVED | Node journal/session/upgrade、sandbox client、managed process metadata | \`agents/node\`、adapters、Kernel daemon |
| SPLIT | Manifest model、semantic contract、Kernel API、daemon 实现 | \`contracts/rust\`、\`kernel/crates/cy-kernel-api\`、\`cy-kernel-daemon\` |
| EXTRACTED | 厂商硬件发现和特权 sandbox 执行 | \`adapters/hardware\`、\`adapters/execution\` |
| REPLACED | 重复 plugin supervisor 和无类型 Agent/AI RPC | InstanceActor/watchdog、通用 Kernel contract、Plugin-owned endpoint |
| DELETED_OBSOLETE | 已退役的 \`cy-plugin-supervisor\)、legacy \`ai_service.proto\`/\`agent_service.proto\` interface | 归档的 migration tooling；无规范 runtime caller |
| LOST_SUSPECTED | 无 | — |

## 正确性矩阵

| Capability | 规范实现和证据 |
|---|---|
| Principal | \`kernel/crates/cy-kernel-daemon/src/peer_cred.rs\)；authority 拒绝缺少 transport-derived identity 的调用。 |
| Worker | \`cy-kernel-api/src/authority.rs\)、daemon \`watchdog/instance_actor.rs\)、\`rpc/worker_control.rs\)；必须提供 generation 和 lease。 |
| Operation | Authority port 加 daemon operation conversion/RPC；生命周期转换由 contract test 覆盖。 |
| Resource | \`cy-resource-manager/src/lib.rs\)；inventory generation、分配和 Lease ownership 均受 fencing 保护。 |
| Lease/Fence | \`cy-kernel-api/src/lease.rs\)、resource manager 和 daemon release/revoke 路径；包含 stale fence 与 cleanup 未完成测试。 |
| Capability | \`framework/crates/cy-platform-api\)；manifest normalization 和 selection 保持通用。 |
| Endpoint | Endpoint/grant authority 绑定 Lease，并会在 Worker 丢失/释放时清理。 |
| Event | Durable event store/history 及 \`WatchEvents\` v1/v2 projection。 |
| Replay/live handoff | \`snapshot\` 在读状态前捕获 cursor；有序 replay 后交接 notifier 已实现并通过 regression test。 |
| Cleanup/recovery | Watchdog、service manager 和 sandbox client 要求物理清理确认；重启恢复按 epoch/fence 限定。 |

当前精确源码没有在生产 Rust 部分命中禁止使用的 Product 名词（dataset/training/model-version/evaluation/deployment/LoRA/tokenizer/product policy）。因此 \`KERNEL_CORRECTNESS_CAPABILITIES_LOST=0\) 和 \`BUSINESS_SEMANTICS_REMAINING_IN_KERNEL=0\) 都是有证据支持的结果。

当前生产 Rust 源码没有业务禁用词命中，因此正确性能力丢失为 0，Kernel 残留业务语义为 0。完整逐项记录、commit 和路径见 JSON 映射。

## 归属图

- Kernel 拥有认证 authority、Lease/Fence、生命周期决策、通用协议校验和 recovery policy。
- Resource Manager 拥有 inventory、allocation、Lease state，并且只在确认 cleanup 后负责 release。
- Platform API 拥有通用 capability manifest normalization 和 selection。
- Package Runtime 拥有 package installation/dependency preparation；它不是第二个 manifest 或 registry authority。
- Contracts 拥有版本化协议/schema/manifest。
- Plugins 拥有官方 package 实现和协议 consumer。
- Product 拥有 binding、policy、auth/UI、connector 语义和领域数据。

Kernel 只拥有通用授权、Lease fencing、生命周期决策、协议校验与恢复策略；Resource Manager、Package Runtime、Contracts、Plugins、Product 各自保留单一 authority。未发现第二个 registry、manifest 或 runtime lifecycle owner。Product 业务 payload 只进入 Plugin 自有 Endpoint。
