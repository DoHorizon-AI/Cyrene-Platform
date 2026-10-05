# Runtime Maintenance Library

This module owns the durable update gate, activity-source catalog, task registry, and broker protocol.

| File | Responsibility |
| --- | --- |
| `lib.rs` | Shared file-locked state, journal recovery, readiness decisions, and admission lifecycle. |
| `main.rs` | Unix-socket broker and one-shot JSON IPC client used by `docker exec`. |

Read `lib.rs` for the state model, then `main.rs` for the broker transport.

## Package binding-operation authority

The root-owned activity catalog may grant exact package-runtime scopes per
source. `binding_scopes` is optional for compatibility and defaults to an empty
allowlist. Each configured scope names one binding, package, exact installation
IDs, and the permitted `activate`, `recover`, and `deactivate` operations.
Wildcard installations and bindings shared by multiple sources are rejected.

`Health` advertises broker protocol `cyrene.runtime-maintenance.broker.v1` and
capability `cyrene.runtime-maintenance.binding-operations.v1`. The
`AdmitBindingOperation` and `CompleteBindingOperation` envelopes must carry that
exact capability string as `protocol_version`. The broker checks the current
catalog generation, source token, Unix peer UID/GID, and the full scope while
holding the shared gate lock. The admission receipt echoes the validated
`catalog_generation`; the Product owner must use that generation when it later
completes the receipt.

## Shared state schema migration

The shared persistence cohort is `cyrene.runtime-maintenance.state.v2`. It is
independent of both the broker wire version (`cyrene.runtime-maintenance.broker.v1`)
and the activity catalog schema, which remains version 1. A normal schema-2
open validates the snapshot and journal and requires a complete migration
marker if one exists. A schema-1 state never upgrades implicitly; an older
writer also cannot open a schema-2 state.

The released schema-1 layout has neither `binding_operations` nor
`completed_binding_operations`. A separately identified experimental layout
has both maps. Migration requires a root-generated proof that names the
verified profile, exact active `CORE_RUNTIME` hold, request and token, plan,
complete component artifact digest map, and the hold's gate/catalog
generations. The profile must come from the signed installed cohort and the
entire snapshot/checkpoint history; callers must not select a profile to make
an unknown state fit. A single missing binding map, unknown field, malformed
journal, profile disagreement, pending task/runtime/binding admission, or
hold mismatch fails closed. Completed binding-operation records are retained.

Before migration, the old broker must create the actual maintenance hold.
Stop the Package Runtime daemon, Product mutation clients, Workspace updater
and timers, and control clients; stop Kernel and wait for it to exit; stop
catalog and other direct state writers; then stop the old broker and verify
its process and socket are gone. With every shared-state writer stopped, run:

```text
cyrene-runtime-maintenance migrate-state --state-dir PATH --maintenance-proof-file PATH
```

The proof file is root-owned, regular, single-link, and mode 0600. Migration
holds `maintenance.lock`, validates the complete schema-1 journal without
repair, saves private source and target backups, and writes
`maintenance-schema-migration.json` through explicit recoverable phases. The
state and journal files remain `maintenance-state.json` and
`maintenance-journal.jsonl`. Known phase/hash combinations can resume with the
same exact proof; unknown or mixed files remain closed for manual repair. The
marker contains the original target hashes and hold identity, but not the
maintenance token. After migration, start the matching v2 broker and Kernel,
verify state-v2 health and the same hold, then end the hold before resuming
clients.

If the first v2 boot fails health, stop all clients and both candidate writers
and run:

```text
cyrene-runtime-maintenance rollback-state --state-dir PATH --maintenance-proof-file PATH
```

Rollback requires the same still-active hold and verifies the original
migration baseline against the full journal. It accepts only compatible
startup metadata writes (source heartbeats, sequential catalog generations,
and empty task reconciliation), preserves those compatible v1 fields and
generation advances, and keeps completed operations and the active hold. Any
new task, runtime admission, active/completed binding record change, hold
transition, journal compaction that loses the baseline, or unknown event
rejects rollback. Start the old broker and Kernel, verify the hold, end it, and
only then resume clients. A COMPLETE marker permits normal v2 state writes;
its target hashes remain the immutable migration/rollback baseline and are not
required to match the live files after startup.

共享持久化 cohort 的版本为 `cyrene.runtime-maintenance.state.v2`。它与
broker wire 版本 `cyrene.runtime-maintenance.broker.v1`、仍保持 v1 的
activity catalog schema 相互独立。普通 schema-2 open 会校验 snapshot、journal，
并检查存在时的 migration marker。schema-1 不会在普通启动时隐式迁移；旧 writer
也不能打开 schema-2 state。

正式发布的 schema-1 布局不含 `binding_operations` 和
`completed_binding_operations`；另有一个明确标识的实验布局同时包含两张 map。
迁移需要 root 生成的 proof，其中绑定经签名安装 cohort 确认的 profile、精确的
active `CORE_RUNTIME` hold、request/token、plan、完整组件 artifact digest map，
以及 hold 的 gate/catalog generation。profile 必须从已验证的签名 cohort 和完整
snapshot/checkpoint 历史推导，调用者不能通过随意选择 profile 让未知状态通过。
只有一张 binding map、未知字段、损坏 journal、profile 不一致、pending task/runtime/
binding admission 或 hold 不匹配都会 fail closed；已完成的 binding-operation 记录
必须保留。

迁移前由旧 broker 建立真实 maintenance hold。依次停止 Package Runtime daemon、
Product mutation clients、Workspace updater/timers 和控制客户端；停止 Kernel 并等待退出；
停止 catalog 与其他直接 state writer；最后停止旧 broker，确认进程和 socket 均已退出。
所有共享 state writer 停止后执行上面的 `migrate-state` 命令。proof 文件必须是
root-owned 普通文件、单链接且权限 0600。迁移持有 `maintenance.lock`，不修复地完整校验
schema-1 journal，保存私有源/目标备份，并通过显式可恢复 phase 写入
`maintenance-schema-migration.json`。state/journal 文件名仍是
`maintenance-state.json` 与 `maintenance-journal.jsonl`。相同 proof 下，已知 phase/hash
组合可继续；未知或混合文件保持关闭并等待人工修复。marker 保存原始目标 hash 和 hold
身份，但不保存 maintenance token。迁移后启动匹配的 v2 broker 与 Kernel，验证 v2 health
及同一 hold，再结束 hold 并恢复客户端。

若首轮 v2 启动未通过 health，停止所有客户端和两个候选 writer，再执行
`rollback-state`。回滚要求同一 hold 仍有效，并根据完整 journal 校验迁移基线。它只接受
兼容的启动元数据写入（source heartbeat、连续递增的 catalog generation、空 task
reconciliation），保留这些可由 v1 表示的字段与 generation 变化，并保留已完成操作和
active hold。新增 task、runtime admission、active/completed binding 记录变化、hold
转换、丢失基线的 journal compaction 或未知事件都会拒绝回滚。恢复旧 broker 与 Kernel
后，验证并结束 hold，再恢复客户端。COMPLETE marker 允许正常 v2 state 写入；其中 target
hash 是不可变迁移/回滚基线，Kernel 启动后的 live 文件无需继续与其相同。

Catalog replacements must use `init-catalog`, which commits the file and its
gate generation under the same lock as admission and completion. A pending
binding operation always rejects replacement. An active maintenance hold also
rejects ordinary catalog changes; the exact hold owner may commit a planned
change by supplying a root-owned, mode-0600 `--maintenance-proof-file` with
`request_id`, `maintenance_token`, `plan_id`, `plan_digest`, and the complete
`component_artifact_digests` map. The read-only `ValidateMaintenanceHold` broker
method checks the current root operator, transaction, plan, component digest,
gate generation, and catalog generation before and after offline installation.
A held catalog generation change does not change the hold's gate generation or
prevent `EndMaintenance` from closing that same transaction.

The daemon reserves before a mutation and returns the admission as a receipt
after recording its activation state. The Product owner persists that receipt
with its binding state before it calls `CompleteBindingOperation`. An admitted
operation remains a separate update blocker across process restart until the
exact owner completes it; task reconciliation and elapsed time cannot remove
it. Only one operation may be pending for a binding at a time: replaying the
same request and scope is idempotent, while another request ID is denied with
`BINDING_OPERATION_ALREADY_INFLIGHT` until the owner completes the earlier
receipt. Different bindings can be active concurrently. A catalog generation
changed outside the locked `init-catalog` path is rejected while an operation
or maintenance hold is active.

根管理的 activity catalog 可以按 source 授予精确的 package-runtime scope。
为兼容旧 catalog，`binding_scopes` 可省略，省略时等同空 allowlist。每项
授权指定一个 binding、package、精确 installation ID 列表，以及允许的
`activate`、`recover`、`deactivate` 操作。catalog 不接受 installation
通配符，也不允许多个 source 共同拥有同一个 binding。

`AdmitBindingOperation` 与 `CompleteBindingOperation` 必须携带已广告的
`cyrene.runtime-maintenance.binding-operations.v1` 请求协议版本。Broker 在
共享 gate 文件锁内检查当前 catalog generation、source token、Unix peer
UID/GID 和完整 scope。Admission receipt 会回传已验证的
`catalog_generation`，Product owner 完成 receipt 时使用该 generation。daemon
在 mutation 前建立 reservation，并在记录自身
activation state 后将 admission 作为 receipt 返回。Product owner 将 receipt
和 binding state 一起持久化后，再调用 `CompleteBindingOperation`。已准入操作
会独立阻止更新，并跨 daemon 重启保留，直到原 owner 按完全相同的 scope
完成；task reconciliation 和等待时间都不能删除该记录。
同一个 binding 同时只允许一个 pending operation。相同 request ID 和 scope
的重放保持幂等；在原 owner 完成前，新的 request ID 返回
`BINDING_OPERATION_ALREADY_INFLIGHT`。不同 binding 仍可并行。pending operation
或 maintenance hold 期间若绕过带锁的 `init-catalog` 路径直接变更 catalog
generation，Gate 会 fail closed。

catalog 替换必须经 `init-catalog`，它在与 admission/completion 相同的锁内提交
catalog 文件及其 gate generation。存在 pending binding operation 时始终拒绝替换。
active maintenance hold 期间普通 catalog 变更也会被拒绝；只有精确 hold owner
提供 root-owned、权限为 0600 的 `--maintenance-proof-file` 才能提交该计划内变更。
证明文件包含 `request_id`、`maintenance_token`、`plan_id`、`plan_digest` 和完整的
`component_artifact_digests` 映射。只读 Broker 方法 `ValidateMaintenanceHold`
会在离线安装前后核验当前 root operator、事务、计划、组件 digest、gate generation
及当前 catalog generation。hold 内 catalog generation 变化不会改变 gate generation，
也不会妨碍 `EndMaintenance` 结束同一事务。
