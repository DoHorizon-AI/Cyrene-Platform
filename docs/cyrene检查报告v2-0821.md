# Implementation Status Audit — v2

Status: **Historical snapshot**. It records an earlier audit checkout and does not describe the current Platform boundary.

审计基准：`docs/contracts/kernel-semantic-contract-v1.md`，Frozen v1.0。  
审计对象：当前工作树，分支 `fix/endpoint-authority-hardening`，`HEAD=e7725b9`。  
对比基线：v1 报告（`HEAD=e725e57`，2026-08-20）。

## 变更提交列表（审计期间）

| SHA | 日期 | 说明 |
|---|---|---|
| `9276d8e` | 08-20 19:50 | refactor(kernel): checkpoint phase 2 authority hardening |
| `03d0911` | 08-21 00:10 | refactor(kernel): complete phase 2 semantic hardening |
| `1e83ce7` | 08-21 14:29 | feat(kernel): complete phase 3 reliability recovery and restart closure |
| `aff27e3` | 08-21 15:05 | fix(kernel-daemon): require physical cleanup before lease reaches RELEASED |
| `e7725b9` | 08-21 16:08 | fix(kernel-daemon): purge endpoint authority when worker lease is released or worker is lost |

---

## A. Executive Summary（对比更新）

```text
Kernel semantic completeness:              68%  (+20%)
Runtime reliability completeness:          65%  (+23%)
Recovery completeness:                     52%  (+34%)
Event model completeness:                  60%  (+22%)
Provider/Capability completeness:          62%  (+22%)
Multi-language service layer completeness: 22%  (±0%)
```

**核心进展：**

1. ✅ **Principal PID 身份违规已修复**：`peer_cred.rs` 改为以稳定 UID/GID 映射 Principal；PID 仅保留为 transport evidence。
2. ✅ **Endpoint Principal 所有权授权已修复**：publish/authorize/revoke 均验证调用 Principal 与 Worker owner 一致；Worker lost/lease released 时自动清除 Endpoint authority。
3. ✅ **Production `KernelAuthority` implementation 已建立**：`LocalKernelAuthority` 在 `kernel-daemon/src/authority.rs` 实现了完整的 `KernelAuthority` trait，tonic 服务退化为纯 projection。
4. ✅ **Namespace 层已引入**：`NamespaceId` 类型、`AuthorityCallContext.namespace`、`namespace_owners` 绑定策略、对象键按命名空间隔离，Core v2 proto 强制携带 `namespace` 字段。
5. ✅ **Provider registration / session generation / per-adapter 隔离已实现**：`register_provider`、`reconcile_provider`、独立 `session_generation`、`last_snapshot_generation`，`ResourceFactsOnly` scope 防止单 adapter 失败影响其他 Provider。
6. 🟡 **Lease release 状态机部分修复**：canonical release 路径已拆分为 `begin_release → RELEASING → complete_release → RELEASED`；legacy stop/watchdog 路径尚未全部前移。
7. ✅ **Worker lost 联动补全（canonical path）**：`mark_worker_lost` 已驱动 Lease revoke、fence advance 和语义事件，并清除 Endpoint authority。
8. ✅ **Recovery 四分类已实现**：`Discover → Classify → Recover → Reconcile → Ready`，FileRuntimeJournal 产出 Valid/Stale/Unknown/Foreign verdict，Stale 进程被 reaped，其余阻塞启动 fail-closed。
9. ✅ **Snapshot + cursor 一致性 API 已实现**：`LocalKernelAuthority::snapshot()` 先捕获事件 cursor 再读取状态，`Snapshot @ C + events_after(C) = current state` 边界成立。
10. ✅ **DurableEventStore 已接入**：semantic event 通过 `DurableEventStore` port 落盘，runtime journal 支持 event replay；cancel_operation 已真正驱动 executor cancellation（通过 `InstanceActor::request_cancel`）。

---

## B. Implementation Matrix（已更新状态）

| Area | 旧状态 | 新状态 | 变化说明 |
|---|---|---|---|
| Identity | 🟡 Partial | 🟡 Partial | 语义身份稳定，但内部部分索引仍按字符串 ID |
| Namespace | ❌ Missing | 🟡 Partial | `NamespaceId` 类型、所有权绑定、Core v2 proto 已有；Semantic contract v1 noun 字段本身无 Namespace 字段（transport 层隔离） |
| Generation | 🟡 Partial | 🟡 Partial | Provider session/snapshot generation 已独立；Lease generation 仍固定 1 |
| Principal | 🔴 Contract Violation | ✅ Fixed | UID/GID policy subject；PID 仅 transport evidence；同 UID/GID 跨 PID 稳定回归测试通过 |
| Provider | 🟠 Skeleton | 🟡 Partial | `register_provider`/`reconcile_provider` 已实现；per-adapter 隔离完成；Provider semantic registration path 存在但仍依赖静态 CLI 接入 |
| Capability | 🟡 Partial | 🟡 Partial | 无变化 |
| Resource | 🟡 Partial | 🟡 Partial | 无变化 |
| Lease | 🔴 Contract Violation | 🟡 Partial | canonical path 已拆分 `RELEASING`；legacy stop/watchdog 路径未完全前移 |
| Fence | 🟡 Partial | 🟡 Partial | Worker lost 路径 fence advance 已接入；启动 adapter 不传 fence 仍存在 |
| Worker | 🟡 Partial | 🟡 Partial | `mark_worker_lost` 路径完整；no restart adoption；删除后不可查仍在 |
| Operation | 🟡 Partial | 🟡 Partial | `cancel_operation` 已驱动 `InstanceActor::request_cancel`；start Operation 与 heartbeat 联动未完成 |
| Endpoint | 🔴 Contract Violation | ✅ Fixed | owner Principal 验证完整；`purge_endpoint_authority` 在 lease released / worker lost 时自动调用；grant 消费验证 data-plane 仍缺 |
| Events | 🟡 Partial | 🟡 Partial | `DurableEventStore` 接入；canonical SubscribeEvents 仍为 unary |
| Snapshot | ❌ Missing | ✅ Fixed | `LocalKernelAuthority::snapshot()` + cursor 一致性边界；`AuthoritySnapshot` 结构完整 |
| Replay | 🟡 Partial | 🟡 Partial | 现有 durable store 支持 replay；GAP/SOURCE_CHANGED 语义完整 |
| Reconciliation | 🟡 Partial | 🟡 Partial | `reconcile_provider`、ResourceFactsOnly、per-adapter 隔离完成；Worker/Lease/Endpoint 完全联动收敛仍在进行 |
| Recovery | 🟠 Skeleton | 🟡 Partial | Valid/Stale/Unknown/Foreign 四分类完整；Stale reap 实现；Valid/Unknown/Foreign 阻塞 fail-closed；不恢复语义权威 |
| Persistence | 🟡 Partial | 🟡 Partial | RuntimeProcessRecord 持久化；DurableEventStore 落盘；Worker/Lease/Operation/Endpoint 仍不重建 |
| Projection | 🔴 Contract Violation | ✅ Fixed | `KernelAuthority for LocalKernelAuthority` 为 production implementation；tonic service 为纯 thin projection |
| Service API | 🟠 Skeleton | 🟠 Skeleton | 无变化 |
| Package/Manifest | 🟡 Partial | 🟡 Partial | 无变化 |

---

## C. Contract Violations（已更新）

### 1. Principal 使用 PID 作为稳定 Identity ✅ 已修复

**提交**：`9276d8e` (`peer_cred.rs`)

- `principal_from_peer_cred` 改为 `id = "unix-principal/uid-{uid}/gid-{gid}"`，`generation = 1`。
- PID 保留在 `PeerCred` struct 供 transport evidence，不再进入语义身份。
- 回归测试：同一 UID/GID 的不同 PID（4242 vs 8181）产生相同 `Principal`，测试通过。

---

### 2. Lease release 跳过 `RELEASING` 🟡 部分修复

**提交**：`aff27e3` (`resource/lib.rs`, `authority_service.rs`)

- canonical `release_lease` 路径已重构为 `begin_release → RELEASING → complete_release → RELEASED`，cleanup 未完成时资源保持 RELEASING，不可重新分配。
- durable `LEASE_RELEASE_STARTED` 日志先于 RELEASING 状态可见。

**尚未解决**：

- legacy stop/watchdog 路径（heartbeat timeout → stop/release/remove）仍可能直接 `Active → Released`，cleanup 决策未前移到物理 cleanup 之前。
- 此路径的状态机合规性测试缺失。

---

### 3. Endpoint authority 不验证调用 Principal ✅ 已修复

**提交**：`9276d8e` (phase 2), `e7725b9` (purge on lost/release)

- `publish_endpoint`、`authorize_endpoint`、`revoke_endpoint` 均从 `KernelAuthority::require_namespace_owner` 验证 Principal，并比对 Worker owner identity。
- `purge_endpoint_authority(worker_identity)` 在 `mark_worker_lost` 和 `complete_release` 时调用，彻底清除 stale Endpoint 和 Grant。
- 回归测试：不同 Principal 操作对方 Worker Endpoint 全部拒绝；Worker lost/replaced 后 Endpoint 不可见。

---

### 4. Semantic state machine 位于 transport adapter ✅ 已修复

**提交**：`03d0911`

- `kernel/crates/cy-kernel-daemon/src/authority.rs` 新增 `impl KernelAuthority for LocalKernelAuthority`，实现了所有 canonical authority RPC 的 transport-independent 状态机。
- `rpc/authority_service.rs` 退化为纯 thin projection：接收请求 → 提取 Principal → 调用 `LocalKernelAuthority` 方法 → 映射错误。
- `KernelServiceAdapter` 不再持有或修改 semantic maps；所有状态通过 `AuthorityRuntime` struct 由 `LocalKernelAuthority` 拥有。

---

### 5. Provider v1 stable actions 被 canonical projection 省略 ✅ 基本修复

**提交**：`1e83ce7` (`provider_service.rs`, `kernel_provider.proto`)

- `RegisterProvider`、`PublishInventory`（等效于 `reconcile_provider`）已在 `KernelProviderAuthority for LocalKernelAuthority` 实现。
- Provider session generation、snapshot generation 和 Resource generation 三层独立。

**尚未解决**：

- Provider dynamic registration（TCP/mTLS 动态接入）仍需 CLI 静态配置接入；`RegisterProvider` gRPC RPC 映射完整，但实际接入点仍为 UDS 静态 adapter。

---

### 6. 静态 adapter 聚合混淆 Provider incarnation 与 inventory generation ✅ 已修复

**提交**：`1e83ce7` (`adapter/mod.rs`)

- `HardwareAdapterTracker` 持有独立 `session_generation` 和 `last_snapshot_generation`。
- adapter disconnect 调用 `reconnect()` 使 `session_generation++`，`last_snapshot_generation = None`。
- `register_provider` 传入的 Provider identity 携带 `session_generation`；Kernel 拒绝 stale generation 的 reconcile。
- `ResourceFactsOnly` scope 确保 adapter 仅刷新资源数据而不参与 Worker reconciliation，防止单 adapter 失败影响其他 Provider 的 Worker 状态。

---

## D. Skeletons（已更新）

| 编号 | 原状态 | 新状态 | 说明 |
|---|---|---|---|
| 1. `KernelAuthority` trait | 无 production impl | ✅ `impl KernelAuthority for LocalKernelAuthority` 完整实现 | `03d0911` |
| 2. `Provider`/`ProviderSnapshot` model | 模型/TCK only | ✅ `register_provider`/`reconcile_provider` production path 完整 | `1e83ce7` |
| 3. `V1_ACTIONS` 的 17 个动作 | 缺 3 个 Provider 动作 | ✅ Provider actions 补齐 | `1e83ce7` |
| 4. `SubscribeEvents` | unary paged | 🟡 仍为 unary；DurableEventStore 支持历史 replay | 非 server streaming |
| 5. `FileRuntimeJournal` | 仅 fence evidence | ✅ `RuntimeProcessRecord` 持久化；`DurableEventStore` 接入；Recovery 四分类 | `1e83ce7` |
| 6. `ProcessHandle.start_time_ticks` | launch 采集；stop 未验证 | 🟡 Recovery classify 使用 cgroup_name + pid + start_time_ticks 三元组验证 | 部分改善 |
| 7. `EndpointGrant::authorizes` | 无 data-plane 消费 | �� 仍无 data-plane consumer 调用该方法 | 未变化 |
| 8. `InstanceActor` restart fields | 有字段无执行 | 🟡 `request_cancel` 已连接；restart adoption 仍无 | 部分改善 |
| 9. `RestartPolicy` manifest | 解析但不执行 | ⬜ 未修复 | 未变化 |
| 10. JVM control plane | 只打印初始化 | 🟡 JVM gRPC Netty 实现（`develop` 分支）；仍需端到端验证 | 部分改善 |
| 11. JVM plugin integration test | `#[ignore]` | 🟡 `#[ignore]` 仍在 | 未变化 |
| 12. Service manifests | schema 无 runtime | ⬜ 未修复 | 未变化 |
| 13. Snapshot + Stream | 两套独立模型 | ✅ `LocalKernelAuthority::snapshot()` + cursor 一致性 API | `1e83ce7` |
| 14. Namespace | 字符串 grammar | 🟡 `NamespaceId` + `namespace_owners` 隔离在 transport 层；semantic noun 字段无 Namespace | `03d0911` |

---

## E. Missing Reliability Paths（已更新）

| Failure path | 原行为 | 新状态 |
|---|---|---|
| Worker process crash | heartbeat timeout → stop/release/remove | 🟡 `mark_worker_lost` → Lease revoke → fence advance → events → Endpoint purge（canonical path 完整）；legacy watchdog path 需确认 |
| Worker timeout | bounded shutdown + cgroup cleanup | 🟡 semantic lifecycle 联动改善；legacy 路径 release 前移未完成 |
| Canonical Operation cancel | 只把 Operation 设为 `CANCELLING` | ✅ `request_cancel` → `InstanceActor`；超时后 `mark_cancelling_operation_lost` |
| Legacy Operation cancel | 真正 stop/reap/release | 🟡 与 canonical Operation 两套模型仍存在 |
| daemon crash | 所有 in-memory state 丢失 | �� RuntimeProcessRecord 持久；Recovery 四分类完成；语义权威不重建 |
| sandboxd crash | Worker 因 `PDEATHSIG` 应终止 | 🟡 未显式 reconcile（未变化） |
| daemon restart | 新空账本 | 🟡 Stale reap；epoch/fence advance；Valid/Unknown/Foreign 阻塞 fail-closed；旧 Lease/Worker 不重建（设计意图） |
| provider disconnect | aggregate probe 整体失败 | ✅ per-adapter 隔离；一个 adapter 失败不影响其他 |
| provider reconnect | poll 成功后发 recovered event | ✅ `session_generation++`；旧 session reconcile 被拒绝 |
| stale process | sandboxd owned cgroup 命名清场 | ✅ Valid/Stale/Unknown/Foreign 四分类；Stale reap；Unknown/Foreign 阻塞 fail-closed |
| foreign process | 非 owned cgroup 不清理 | ✅ 路径方向安全（未变化） |
| PID reuse | live sandboxd child 用 pidfd | 🟡 Recovery classify 使用三元组；restart 后 stale 进程分类覆盖 PID reuse 场景 |
| event disconnect | 客户端可再次 pull cursor | ✅ DurableEventStore replay；GAP/SOURCE_CHANGED 语义完整 |
| event cursor expiry | 返回 GAP | ✅（未变化） |
| event source restart | 返回 SOURCE_CHANGED | ✅（未变化） |
| slow canonical subscriber | 无 live stream | ⚪ 不适用（未变化） |
| slow legacy subscriber | bounded broadcast/mpsc，lag 后断开 | ✅（未变化） |
| Lease expiry | 惰性回收 | 🟡 无主动 timer/event（未变化） |
| Endpoint after lease expiry | grant map 仍在 | ✅ `purge_endpoint_authority` 在 lease released 时调用 |
| journal failure on acquire/release | fail-closed | ✅（未变化） |
| journal failure on Worker launch/termination | 仅 `eprintln!` | 🟡 recovery evidence 可缺失（未变化） |

---

## F. Test Gap（已更新）

### 已有覆盖（对比更新）

| Golden Test | 旧状态 | 新状态 | 说明 |
|---|---|---|---|
| Stale Fence Test | 🟡 Partial | 🟡 Partial | 新增 stale-generation / corrupted-frame fail-closed 测试；端到端 old Worker recovery 仍需覆盖 |
| Event Resume Test | 🟡 Partial | 🟡 Partial | DurableEventStore + durable replay 回归测试；daemon disconnect/reconnect 生产测试仍缺 |
| Cursor Expiry Test | 🟡 Partial | 🟡 Partial | GAP/SOURCE_CHANGED 语义测试完整；production eviction RPC integration 仍缺 |
| Restart Recovery Test | 🟡 Partial | ✅ 覆盖 | 两 epoch 重启 golden test：epoch N snapshot → restart epoch N+1 → SOURCE_CHANGED → fresh snapshot 无旧权威 → replacement fence 严格递增 |
| Snapshot Atomicity Test | ❌ Missing | ✅ 覆盖 | `snapshot_cursor_and_durable_event_ordering_share_one_boundary` + `snapshot_stays_consistent_while_operations_mutate_concurrently` |
| Endpoint Ownership Authorization Test | ❌ Missing | ✅ 覆盖 | 不同 Principal 操作对方 Worker Endpoint 全部拒绝；Worker lost purge 验证 |
| Principal Stability Test | ❌ Missing | ✅ 覆盖 | 同 UID/GID 不同 PID 产生相同 Principal 回归测试 |
| Namespace Collision Test | ❌ Missing | 🟡 Partial | `namespace_owners` 绑定策略隔离测试存在；tenant 隔离全路径 e2e 未覆盖 |
| Transport Replacement Test | 🟡 Partial | ✅ 覆盖 | `LocalKernelAuthority` 完全脱离 tonic；projection 仅做 mapping |
| Slow Consumer Test | 🟡 Partial | 🟡 Partial | 未变化 |
| New Hardware Test | 🟡 Partial | 🟡 Partial | per-adapter 隔离测试完整；Provider 动态注册 e2e 仍缺 |
| Namespace Isolation Test | ❌ Missing | 🟡 Partial | namespace 存在但 semantic noun 字段不携带；全路径隔离 e2e 未覆盖 |
| JVM/Rust Replacement Test | 🟡 Partial | 🟡 Partial | `#[ignore]` 仍在；JVM gRPC 实现完成但端到端验证缺 |

### 最高优先级缺失测试（剩余）

1. **Legacy watchdog 路径 Lease release 合规测试**  
   heartbeat timeout → cgroup kill → 验证 ACTIVE → RELEASING → RELEASED 顺序；cleanup 未完成时 RELEASED 不可见。

2. **Provider 动态注册 e2e 测试**  
   通过 gRPC `RegisterProvider` + `ReconcileProvider` 接入新 Provider，验证 Lease 可分配、Worker 可启动、Provider 断开后其 Lease 被隔离拒绝。

3. **全 namespace 隔离测试**  
   两个不同 `NamespaceId` 的 tenant 互相不可见 Worker/Lease/Operation/Endpoint/Event。

4. **End-to-end Worker lost reconciliation**  
   杀 Worker 进程（不走正常 stop），验证 `Worker LOST → Lease REVOKED → fence advance → Endpoint purge → events → resource availability`。

5. **Lease expiry active timer**  
   TTL 到期时主动驱动 Worker 终止和 Lease revoke（目前为惰性回收）。

6. **production event eviction/resume**  
   经 authority UDS 生成超过 DurableEventStore 保留窗口的 event，验证 CURRENT/GAP/SOURCE_CHANGED 无静默跳转。

7. **JVM/Rust 跨语言替换测试**  
   当前 `#[ignore]`，需要真实 JVM fixture 和可运行 sandboxd。

---

## G. P0 / P1 / P2 开发计划（已更新）

### P0 — 大部分已解决

| 原 P0 项 | 状态 |
|---|---|
| 去除 Principal PID 语义依赖 | ✅ 已修复（`9276d8e`） |
| Namespace 模型（传输层） | ✅ NamespaceId + namespace_owners（`03d0911`） |
| transport-independent `KernelAuthority` impl | ✅ `LocalKernelAuthority`（`03d0911`） |
| Lease release 状态机拆分 | 🟡 canonical 完成；legacy 路径待修 |
| Endpoint Principal 所有权授权 | ✅ 已修复（`9276d8e` + `e7725b9`） |
| Provider registration/session/reconcile | ✅ 已完成（`1e83ce7`） |
| Worker lost → Lease revoke → fence advance 联动 | ✅ canonical path 完整（`03d0911`） |
| Runtime journal 非关键写失败处理 | 🟡 仍 `eprintln!` 降级（未变化） |
| canonical 与 legacy Operation/Events 迁移边界 | 🟡 两套模型共存（未变化） |

### P1 — 部分完成

| 原 P1 项 | 状态 |
|---|---|
| Provider/Resource/Lease/Worker/Operation/Endpoint 持久化或安全重建 | 🟡 RuntimeProcessRecord + DurableEventStore；语义权威不重建 |
| ProviderSnapshot TTL + per-Provider reconciliation | ✅ `ResourceFactsOnly` + session_generation（`1e83ce7`） |
| Worker 可查询状态（不删除终态） | 🟡 未变化 |
| Operation 与 executor lifecycle/cancel 连接 | ✅ `request_cancel` → InstanceActor（`03d0911`） |
| Lease revoke/replace + monotonic fence durable | ✅ `mark_worker_lost` → revoke → fence advance（`03d0911`） |
| canonical ordered replayable event log | 🟡 DurableEventStore 接入；at-least-once 和 retention 策略待定 |
| Kernel snapshot + cursor 一致性接口 | ✅ `LocalKernelAuthority::snapshot()`（`1e83ce7`） |
| Discover/Classify/Recover（valid/stale/unknown/foreign） | ✅ 四分类完整（`1e83ce7`） |
| sandboxd owned-cgroup cleanup 纳入 recovery result | ✅ Stale reap 在 recover_before_listeners（`1e83ce7`） |

### P2 — 未变化

上层 Service API 演进、语言无关 Invoke/Result/Error schema、`cy.json` 最小 manifest、JVM 真实 Worker fixture 均未推进。

---

## H. 综合结论

本次提交（5 个，约 8000 行新增）解决了 v1 报告中 6 个 Contract Violation 中的 **4 个完整修复**，以及 1 个部分修复（Lease release legacy 路径），并清除了 P0 级别的全部重点架构问题（KernelAuthority projection、Principal PID、Provider registration、Snapshot API、Recovery 四分类）。

**仍需跟进的问题（按优先级）：**

1. 🟡 **Legacy watchdog/stop 路径 Lease release 状态机**：需要将 cleanup 决策前移，并补测试。
2. 🟡 **canonical 与 legacy Operation/Event 两套模型共存**：迁移边界定义和清理计划。
3. 🟡 **Lease 主动过期 timer**：TTL 到期时主动驱动 Worker 终止。
4. 🟡 **全 Namespace 隔离端到端测试**：当前语义 noun 字段无 namespace，tenant 全路径隔离未经测试。
5. 🟡 **JVM worker lifecycle test**：`#[ignore]` 未解除。
6. 🟡 **Runtime journal 写失败静默降级**：关键 transition 的 durable-before-visible 策略未确定。
7. ⬜ **P2 上层能力体系**：Service API schema 演进、语言中立 invoke/result 接口。
