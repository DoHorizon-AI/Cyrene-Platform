# Kernel Reliability Recovery

Phase 3 keeps recovery evidence separate from semantic authority. A restarted Kernel never adopts a pre-crash Worker
merely because a process record exists. It raises the fence floor, discovers Provider/runtime reality, classifies the
record, and reconciles from the normal authority path.

| Fact                                              | Recovery classification                                                              | Storage / source                       |
|---------------------------------------------------|--------------------------------------------------------------------------------------|----------------------------------------|
| Provider logical identity and session generation  | reconstruct from Provider re-registration; stale sessions cannot overwrite a new one | Provider registration                  |
| Provider inventory snapshot generation            | reconstruct from a current Provider observation; obsolete after a session reconnect  | Provider observation                   |
| Resource identity and generation                  | reconstruct from current Provider inventory                                          | Provider observation                   |
| Lease state and fence                             | must persist; fence floor is never reused                                            | runtime journal plus resource ledger   |
| Worker identity, generation, and runtime evidence | persist as unclosed runtime evidence; never auto-adopt                               | runtime journal `RuntimeProcessRecord` |
| Operation terminal state                          | must persist before terminal event visibility                                        | durable semantic event store           |
| Endpoint authority metadata                       | reconstruct only from a current Worker and active Lease; stale data is revoked       | reconciliation                         |
| Event source and cursor                           | source epoch and event sequence persist with each durable event                      | durable event store                    |

Durable-before-visible boundaries are lease acquisition, lease revoke/fence advance, Worker LOST, operation terminal
transition, and semantic event publication. The JSONL composition adapter calls `sync_data` after each append; the
Kernel depends only on `RuntimeJournalSink` and `DurableEventStore` ports.

## Restart policy: Discover → Classify → Recover → Reconcile → Ready

On restart the composition root advances the Kernel epoch before opening authority listeners, seeds the resource
manager above every durable fence, and discovers unclosed runtime records. Those records are stale evidence, not
authority. Provider inventory refresh and Provider-scoped reconciliation then decide whether a Worker is missing,
stale, or still valid under a newly issued authority. A changed Kernel epoch changes the event source, so clients
obtain a fresh authority snapshot before resuming a cursor.

1. **Discover** — advance `node_epoch` in the runtime journal (`begin_epoch`), recover the durable fence floor and
   unclosed `RuntimeProcessRecord` evidence, and probe current hardware/Provider reality. The fence floor is taken
   from the durably persisted journal (`recover()` returns `max(historical fence) + 1`); a fresh manager seeded with
   that floor allocates a strictly greater token than any lease that existed before the restart.
2. **Classify** — `FileRuntimeJournal::classify_recovery` maps discovered runtime evidence against recorded
   `RuntimeProcessRecord`s into one of four verdicts: `Valid`, `Stale`, `Unknown`, `Foreign`. Classification is
   evidence review only; it never rehydrates a Worker into the new process.
3. **Recover** — `recover_before_listeners` runs before any listener opens. `Stale` processes with exactly matched
   sandbox evidence are reaped and journaled terminal. `Valid`, `Unknown`, and `Foreign` processes are never adopted
   or killed by the Kernel; their presence blocks startup fail-closed for operator action
   (`RECOVERY_VALID_PROCESS_UNADOPTED`, `RECOVERY_UNKNOWN_PROCESS`, `RECOVERY_FOREIGN_PROCESS`). A valid old managed
   process therefore never becomes a permanent orphan: it is surfaced for operator decision rather than silently
   re-owned or silently left running under new authority.
4. **Reconcile** — Provider re-registration, inventory observation, and the normal `reconcile_provider` path rebuild
   Worker/Lease/Operation/Endpoint authority from current reality. A `ResourceFactsOnly` provider refreshes only
   resource facts and never participates in Worker reconciliation. Per-adapter Provider isolation means one adapter
   failing to `ADAPTER_UNAVAILABLE` does not hide another adapter's facts, and a reconnect invalidates only the
   session-bound evidence, not resource or snapshot numbers.
5. **Ready** — once reconciliation converges (no pending `TerminateStaleWorker`), the Kernel serves a fresh snapshot
   whose `source` generation equals the new epoch and whose `cursor` starts at the new epoch's sequence. Old
   authority is never restored: the fresh snapshot contains no old Lease/Worker/Operation/Endpoint, and any replacement
   allocation is issued with a fence strictly greater than the old fence.

## Durable event source and cursor semantics

For a source that has durable event history, that ordered history is the replay and snapshot-cursor source of truth;
the in-process 256-event queue is only a cache for stores that do not support history reads. JSONL replay reads are
serialized with appends, require an exact `(namespace, source)` match, and reject malformed or out-of-order durable
records. A read error or corruption fails closed: the Kernel must not fall back to cached events or use historical
events to rebuild active authority state. Events from an older epoch remain audit history only; a new epoch starts with
newly reconciled authority.

`Snapshot @ C + read_events(C) = current state` is the single consistency boundary. `LocalKernelAuthority::snapshot`
captures the event cursor **before** reading authority state, and `read_events` replays from that cursor using the
same `DurableEventStore` ordering. Because a transition publishes its durable event only after mutating state, the
snapshot is always a superset of every event already counted in the cursor, so a concurrent transition can never vanish
from both the snapshot and the incremental replay.

`EventCursor::status_against` defines three outcomes for `read_events`:

| Status         | Condition                                                            | Client action                                                                 |
|----------------|----------------------------------------------------------------------|-------------------------------------------------------------------------------|
| `Current`      | cursor source matches and no gap below the retained history         | apply returned events; persist `next_sequence` as the new cursor              |
| `Gap`          | retained durable history no longer contains `cursor.sequence + 1`    | discard the incremental model; `GetSnapshot`; persist the new cursor; resume  |
| `SourceChanged`| cursor source (epoch/generation) differs from the live Kernel       | discard the incremental model; `GetSnapshot`; persist the new cursor; resume  |

## Client recovery: GAP / SOURCE_CHANGED

A client that receives `GAP` or `SOURCE_CHANGED` discards its incremental model, obtains the existing
`LocalKernelAuthority::snapshot`, reconciles that snapshot, persists its fresh cursor, and resumes `read_events` from
it. `GAP` means the cursor predates retained durable history; `SOURCE_CHANGED` means the cursor belongs to another
Kernel epoch (e.g. after a two-epoch restart). A durable replay read failure is not resumable from memory and requires
the durable store to be repaired or made available before rebuilding client state.

### Client recovery matrix

| Signal                                  | Incremental model | New cursor | Next client step                                  |
|-----------------------------------------|------------------|------------|---------------------------------------------------|
| `read_events` returns `Current`        | keep             | `next_sequence` | persist cursor; continue                         |
| `read_events` returns `Gap`            | discard          | `snapshot.cursor` | `GetSnapshot`; persist; resume `read_events`    |
| `read_events` returns `SourceChanged`  | discard          | `snapshot.cursor` | `GetSnapshot`; persist; resume `read_events`     |
| durable replay read error               | discard          | —          | repair/restore durable store; then `GetSnapshot`  |

The two-epoch restart golden test proves this end to end: an epoch N snapshot carrying a lease, Worker, Operation, and
Endpoint; a persisted cursor; restart into epoch N+1 where the old cursor yields `SourceChanged`; a fresh snapshot
carrying no old authority; and a replacement allocation whose fence exceeds the old fence.
---

<!-- Chinese Translation / 中文翻译 -->

# Kernel 可靠性恢复

Phase 3 将恢复证据与语义权威分开。Kernel 重启后，不能仅因进程记录存在就接管崩溃前的 Worker。它会提高 fence 下限、发现 Provider/runtime 的当前实际状态、对记录分类，并通过常规权威路径进行协调。

| 事实 | 恢复分类 | 存储 / 来源 |
|---|---|---|
| Provider 逻辑身份和 session generation | 根据 Provider 重新注册重建；旧 session 不得覆盖新 session | Provider 注册记录 |
| Provider inventory 快照 generation | 根据当前 Provider 观测重建；session 重连后旧数据失效 | Provider 观测 |
| 资源身份和 generation | 根据当前 Provider inventory 重建 | Provider 观测 |
| Lease 状态和 fence | 必须持久化；fence 下限永不复用 | runtime journal 加资源账本 |
| Worker 身份、generation 和 runtime 证据 | 持久保存为尚未关闭的 runtime 证据；绝不自动接管 | runtime journal 中的 `RuntimeProcessRecord` |
| Operation 终态 | 在终态事件可见前必须持久化 | 持久化语义事件存储 |
| Endpoint 权威元数据 | 仅能根据当前 Worker 和活动 Lease 重建；旧数据应撤销 | reconciliation |
| Event 来源和 cursor | 每个持久事件都保存 source epoch 和 event sequence | 持久化事件存储 |

以下边界必须先持久化再对外可见：获取 Lease、撤销 Lease/推进 fence、Worker LOST、Operation 进入终态，以及发布语义事件。JSONL 组合适配器在每次追加后调用 `sync_data`；Kernel 仅依赖 `RuntimeJournalSink` 和 `DurableEventStore` 端口。

## 重启策略：Discover → Classify → Recover → Reconcile → Ready

重启时，组合根会在打开 authority listener 前推进 Kernel epoch，以高于所有持久 fence 的值初始化 resource manager，并发现尚未关闭的 runtime 记录。这些记录只是过期证据，不是权威。之后，Provider inventory 刷新与 Provider 作用域的协调决定某个 Worker 是缺失、已过期，还是仍受新签发的权威保护。Kernel epoch 改变会使 event source 改变，所以客户端必须先获取新的权威快照，再继续使用 cursor。

1. **Discover** —— 在 runtime journal 中推进 `node_epoch`（`begin_epoch`），恢复持久化的 fence 下限和未关闭的 `RuntimeProcessRecord` 证据，并探测当前硬件/Provider 实际状态。fence 下限取自持久 journal（`recover()` 返回“历史最大 fence + 1”）；以该下限初始化的新 manager 分配的 token 必然严格大于重启前存在的所有 Lease token。
2. **Classify** —— `FileRuntimeJournal::classify_recovery` 将发现的 runtime 证据与已记录的 `RuntimeProcessRecord` 对照，给出四种 verdict 之一：`Valid`、`Stale`、`Unknown`、`Foreign`。分类只是证据审查，绝不在新进程中重新水合 Worker。
3. **Recover** —— `recover_before_listeners` 在任何 listener 打开前运行。只有 sandbox 证据完全匹配的 `Stale` 进程才会被回收并写入终态 journal。Kernel 绝不会接管或杀死 `Valid`、`Unknown`、`Foreign` 进程；这些进程会以失败关闭方式阻止启动，并要求运维人员处理（`RECOVERY_VALID_PROCESS_UNADOPTED`、`RECOVERY_UNKNOWN_PROCESS`、`RECOVERY_FOREIGN_PROCESS`）。因此，仍有效的旧受管进程不会永久成为孤儿：系统会把它交给运维决策，而不会在新权威下悄悄重新归属或任其运行。
4. **Reconcile** —— Provider 重新注册、inventory 观测和常规 `reconcile_provider` 流程依据当前实际状态重建 Worker/Lease/Operation/Endpoint 权威。`ResourceFactsOnly` provider 只刷新资源事实，不参与 Worker reconciliation。各 adapter 的 Provider 隔离意味着某个 adapter 故障并显示为 `ADAPTER_UNAVAILABLE` 时，不会隐藏其他 adapter 的事实；重连只会使绑定 session 的证据失效，不会改变资源或快照编号。
5. **Ready** —— reconciliation 收敛后（不再存在待处理的 `TerminateStaleWorker`），Kernel 才提供新快照，其 `source` generation 等于新 epoch，`cursor` 从新 epoch 的 sequence 开始。旧权威不会被恢复：新快照不含旧 Lease/Worker/Operation/Endpoint，任何替代分配都会获得严格大于旧 fence 的新 fence。

## 持久事件来源与 cursor 语义

对具有持久事件历史的来源，按序排列的历史记录是 replay 和快照 cursor 的事实来源；进程内的 256-event 队列仅是那些不支持历史读取的存储之缓存。JSONL replay 读取会与追加操作串行执行，要求精确匹配 `(namespace, source)`，并拒绝格式错误或顺序颠倒的持久记录。读取失败或数据损坏时必须失败关闭：Kernel 不得回退到缓存事件，也不得利用历史事件重建活动权威状态。旧 epoch 的事件仅作审计记录；新 epoch 以新近协调出的权威状态启动。

`Snapshot @ C + read_events(C) = current state` 是唯一的一致性边界。`LocalKernelAuthority::snapshot` 会在读取权威状态之前捕获 event cursor；`read_events` 再使用同一个 `DurableEventStore` 的顺序，从该 cursor 开始 replay。状态转换先修改状态、随后才发布持久事件，因此快照总是包含 cursor 已计入的所有事件；并发转换不会同时从快照和增量 replay 中消失。

`EventCursor::status_against` 为 `read_events` 定义三种结果：

| 状态 | 条件 | 客户端操作 |
|---|---|---|
| `Current` | cursor source 相符，且保留历史中没有缺口 | 应用返回事件；将 `next_sequence` 持久化为新 cursor |
| `Gap` | 保留的持久历史中不再包含 `cursor.sequence + 1` | 丢弃增量模型；调用 `GetSnapshot`；持久化新 cursor；继续读取 |
| `SourceChanged` | cursor source（epoch/generation）与当前 Kernel 不同 | 丢弃增量模型；调用 `GetSnapshot`；持久化新 cursor；继续读取 |

## 客户端恢复：GAP / SOURCE_CHANGED

客户端收到 `GAP` 或 `SOURCE_CHANGED` 时，应丢弃增量模型，获取现有的 `LocalKernelAuthority::snapshot`，据此重新协调本地状态，持久化新的 cursor，然后从该位置恢复 `read_events`。`GAP` 表示 cursor 早于仍保留的持久历史；`SOURCE_CHANGED` 表示 cursor 属于另一个 Kernel epoch（例如经历了双 epoch 重启）。持久 replay 读取失败时，不能从内存恢复；必须先修复或恢复持久存储，再重建客户端状态。

### 客户端恢复矩阵

| 信号 | 增量模型 | 新 cursor | 客户端下一步 |
|---|---|---|---|
| `read_events` 返回 `Current` | 保留 | `next_sequence` | 持久化 cursor 并继续 |
| `read_events` 返回 `Gap` | 丢弃 | `snapshot.cursor` | 调用 `GetSnapshot`；持久化后恢复 `read_events` |
| `read_events` 返回 `SourceChanged` | 丢弃 | `snapshot.cursor` | 调用 `GetSnapshot`；持久化后恢复 `read_events` |
| 持久 replay 读取错误 | 丢弃 | — | 修复/恢复持久存储，然后调用 `GetSnapshot` |

双 epoch 重启 golden test 对完整流程进行了验证：epoch N 的快照携带 Lease、Worker、Operation 和 Endpoint；cursor 已持久化；重启进入 epoch N+1 后，旧 cursor 返回 `SourceChanged`；新快照不含旧权威；替代分配获得高于旧 fence 的 fence。
