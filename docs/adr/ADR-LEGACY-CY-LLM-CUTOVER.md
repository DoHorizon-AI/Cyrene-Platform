# ADR-LEGACY-CY-LLM-CUTOVER: Internal Legacy Contract Cutover

- Status: Implemented / Historical
- Date: 2026-08-10
- Scope: former version-0 Proto files, generated Rust binding, and Node Agent
  service scaffold

## Decision

The former version-0 package was an internal migration artifact, not a public
compatibility contract. P0 froze it: no new RPC, message, field, command,
consumer, or business behavior could be added.

P1 replaced the public Rust surface with `cyrene::core::v1`, migrated Node
Agent state to the transport-free `NodeControlSession`, and removed the old
Proto files, generated binding, and transition allowlist. Any reintroduction
of the former names in runtime or contract source is now a governance
failure.

The separate `cy.plugin.v1` stdio protocol was outside the original cutover and
was removed later when Plugins-owned direct endpoints replaced it.

## Non-goals

This historical ADR does not define Core v1 service behavior, network
transport, mTLS/UDS, lease admission, sandboxing, or plugin startup. Those are
P2/P3 work.

## Deletion gate

The P1 deletion gate was satisfied when:

1. Core v1 contracts and generated bindings build from a clean checkout;
2. cy-node-agent uses the replacement contract and has no remote command echo;
3. no runtime or contract source path contains the former legacy references;
4. Rust CI and the replacement fixture tests pass;
5. deletion is one local reviewed cutover commit after the Core v1 baseline.

## Legacy-surface Closure (2026-09)

The repository-wide legacy-surface closure extended the above cutover to every
Cyrene repo. Scope and rules (authority baseline):

- **One concept = one authority.** Platform (execution/device/lease/Worker/Artifact
  Plane), Catalyst (Dataset/DatasetVersion/Preparation/feedback), Yield
  (TrainingRun/TrainingResult/ModelVersion — **single canonical ModelVersion
  authority**), Reactor (Deployment/Endpoint/serving), Exchange
  (Route/API protocol/auth/quota), Navigator (Conversation/AgentRun/UX/session —
  no second-message authority), Echo (Evaluation/HumanAnnotation/FeedbackSet).
  Kernel is transport-neutral.
- **Classification before deletion.** Every legacy object was classified
  (DEAD / SUPERSEDED / VALUABLE_MISSING / ACTIVE_COMPATIBILITY / SUPPORTED_MIGRATION
  / TEST_HISTORICAL_DEPENDENCY / REFERENCE_ONLY / GENERATED_LEGACY_SCHEMA /
  AMBIGUOUS_AUTHORITY) prior to any destructive action.
- **VALUABLE_MISSING → reimplemented fresh** under the canonical tree; legacy was
  never copied/imported (e.g. Echo `AnalyticsPanel` session-analytics, Exchange
  `0001_gateway_pro.sql` auth/quota schema).
- **Phase 0 preservation closure** ran first: all 9 `pre-canonical-sync` stashes
  archived and dropped; `.worktrees/` clutter archived (sanitized) and removed;
  `UNRECOVERABLE_LOCAL_WORK = 0` before any deletion.

### Per-repo outcome (PRs)
| Repo | Action | PR |
|---|---|---|
| Cyrene-Echo | delete `legacy/navigator-feedback/*` | #6 (+issue #7 reimplement) |
| Cyrene-Reactor | delete legacy stub + dead `tests/legacy/*`; move `test_checkpoint_digest.py` | #11 |
| Cyrene-Exchange | delete `legacy/*`; decouple `service.json` | #12 (+issue #13 reimplement) |
| Cyrene-Navigator | delete `legacy-dh/` (183 files) | #5 |
| Cyrene-Catalyst | move 30MB corpus → DH-LLMs; keep minimal sample; decouple `service.json` | #6 |
| Cyrene-Platform | delete `tooling/migration/` museum (38,702 LOC) | #36 |
| Cyrene-Plugins-Official | delete `archive/` stubs; **preserve** `compatibility/` (decision D) | #10 |
| Cyrene-Yield | keep `test_kernel_legacy_guard.py`; flagged `archive/icy-lunar-llamafactory` (remote) | — |
| Astrbot-Rev | no dead legacy (`CompatibilityBridge` is live) | — |
| DH-System-Internal | no legacy surface (decision B not triggered) | — |

## Legacy-reintroduction guard

To keep `archive/legacy/backup = 0` and prevent silent resurrection:

1. **Path-level CI check** fails if any of these patterns reappear in committed
   source: `^legacy/`, `^archive/`, `test_legacy_`, `plugin.legacy.toml`,
   `LEGACY_PLUGIN.md`, `*.legacy.toml`, `legacy-requirements/` (outside the two
   enterprise Dockerfiles that still consume it). Reference implementation:
   `tooling/ci/check-no-legacy-surface.sh` (wire into `architecture-governance.yml`
   once all repos stabilize).
2. **`compatibility/` is an explicit, governed adapter layer** — NOT a loophole.
   The conformance validator already rejects generic-implementation paths inside
   `compatibility/` and any import of `astrbot`/Yield/Reactor/Exchange/Navigator/
   Catalyst/Echo from generic code. Keep this guard; do not relax it while
   `compatibility/` shims remain.
3. **Authority registry** (`plugin-boundaries.json`) is the single source of truth
   for capability ownership. A new implementation must not re-introduce a second
   authority for an already-owned concept.
4. **VALUABLE_MISSING reimplementation** must land as a fresh canonical module with
   tests; never as a resurrection of the deleted legacy path.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-LEGACY-CY-LLM-CUTOVER：内部旧契约切换

- 状态：Implemented / Historical
- 日期：2026-08-10
- 范围：旧 version-0 Proto 文件、生成的 Rust binding 和 Node Agent service scaffold

## 决策

原 version-0 package 是内部迁移制品，不是公开兼容契约。P0 冻结了它：不得新增 RPC、message、field、command、consumer 或业务行为。

P1 将公开 Rust surface 替换为 `cyrene::core::v1`，把 Node Agent 状态迁移到与 transport 无关的 `NodeControlSession`，并移除旧 Proto 文件、生成 binding 和迁移 allowlist。Runtime 或 contract source 重新引入旧名称属于治理失败。

单独的 `cy.plugin.v1` stdio 协议不属于最初切换范围；后来在 Plugins-owned direct endpoint 替代它时，该协议也被移除。

## 非目标

本历史 ADR 不定义 Core v1 service 行为、网络传输、mTLS/UDS、Lease 准入、sandboxing 或 plugin startup。这些属于 P2/P3 工作。

## 删除 Gate

满足以下条件后，P1 删除 gate 即视为通过：

1. Core v1 contract 和生成 binding 可从干净检出构建；
2. cy-node-agent 使用替代契约，且不存在远程 command echo；
3. runtime 或 contract source 路径中不含原旧版引用；
4. Rust CI 和替代 fixture test 通过；
5. 删除操作是在 Core v1 baseline 后一次本地审查的 cutover commit 中完成。

## 旧接口全面封闭（2026-09）

全仓库旧接口封闭将上述切换扩展至每个 Cyrene 仓库。范围和规则如下（authority baseline）：

- **一个概念只有一个权威方。**Platform 负责 execution/device/lease/Worker/Artifact Plane；Catalyst 负责 Dataset/DatasetVersion/Preparation/feedback；Yield 负责 TrainingRun/TrainingResult/ModelVersion（**ModelVersion 只有一个规范 authority**）；Reactor 负责 Deployment/Endpoint/serving；Exchange 负责 Route/API protocol/auth/quota；Navigator 负责 Conversation/AgentRun/UX/session（不得出现第二套 message authority）；Echo 负责 Evaluation/HumanAnnotation/FeedbackSet。Kernel 与 transport 无关。
- **先分类，再删除。**所有旧对象在任何破坏性操作前都必须分类为 DEAD、SUPERSEDED、VALUABLE_MISSING、ACTIVE_COMPATIBILITY、SUPPORTED_MIGRATION、TEST_HISTORICAL_DEPENDENCY、REFERENCE_ONLY、GENERATED_LEGACY_SCHEMA 或 AMBIGUOUS_AUTHORITY。
- **VALUABLE_MISSING → 在规范位置重新实现。**必须在 canonical tree 中重新实现，绝不复制/导入旧实现（例如 Echo 的 `AnalyticsPanel` session analytics、Exchange 的 `0001_gateway_pro.sql` auth/quota schema）。
- **先完成 Phase 0 preservation closure。**先归档并删除全部 9 个 `pre-canonical-sync` stash；归档（经过清理）并删除 `.worktrees/` 杂项；任何删除前，`UNRECOVERABLE_LOCAL_WORK = 0`。

### 各仓库结果（PR）

| 仓库 | 操作 | PR |
|---|---|---|
| Cyrene-Echo | 删除 `legacy/navigator-feedback/*` | #6（另有重实现 issue #7） |
| Cyrene-Reactor | 删除旧 stub 和无效的 `tests/legacy/*`；移动 `test_checkpoint_digest.py` | #11 |
| Cyrene-Exchange | 删除 `legacy/*`；解耦 `service.json` | #12（另有重实现 issue #13） |
| Cyrene-Navigator | 删除 `legacy-dh/`（183 个文件） | #5 |
| Cyrene-Catalyst | 将 30MB corpus 移到 DH-LLMs；保留最小样本；解耦 `service.json` | #6 |
| Cyrene-Platform | 删除 `tooling/migration/` museum（38,702 LOC） | #36 |
| Cyrene-Plugins-Official | 删除 `archive/` stub；**保留** `compatibility/`（决策 D） | #10 |
| Cyrene-Yield | 保留 `test_kernel_legacy_guard.py`；标记远端的 `archive/icy-lunar-llamafactory` | — |
| Astrbot-Rev | 无无效旧代码（`CompatibilityBridge` 仍在使用） | — |
| DH-System-Internal | 无旧接口面（未触发决策 B） | — |

## 防止旧接口重新引入

为保持 `archive/legacy/backup = 0` 并防止旧代码悄悄复活：

1. **路径级 CI 检查**：若已提交源码中重新出现以下模式则失败：`^legacy/`、`^archive/`、`test_legacy_`、`plugin.legacy.toml`、`LEGACY_PLUGIN.md`、`*.legacy.toml`、`legacy-requirements/`（仍被两个 enterprise Dockerfile 使用的目录除外）。参考实现为 `tooling/ci/check-no-legacy-surface.sh`；所有仓库稳定后应接入 `architecture-governance.yml`。
2. **`compatibility/` 是显式受治理的 adapter 层**，不是漏洞。conformance validator 已拒绝其中的通用实现路径，以及通用代码对 `astrbot`/Yield/Reactor/Exchange/Navigator/Catalyst/Echo 的 import。在 compatibility shim 仍存在时必须保留该检查，不得放宽。
3. **Authority registry**（`plugin-boundaries.json`）是 capability 归属的单一事实来源。新增实现不得给已归属概念引入第二个 authority。
4. **VALUABLE_MISSING 重实现**必须作为全新的 canonical module 并带有测试；不得复活已删除的旧路径。
