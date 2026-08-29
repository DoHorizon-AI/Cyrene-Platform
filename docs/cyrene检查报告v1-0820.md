# Implementation Status Audit

审计基准：`docs/contracts/kernel-semantic-contract-v1.md`，Frozen v1.0。  
审计对象：当前工作树，分支 `develop-kernel`，`HEAD=e725e57`，包含 12 个未提交改动路径。未修改代码。

## A. Executive Summary

```text
Kernel semantic completeness:              48%
Runtime reliability completeness:          42%
Recovery completeness:                     18%
Event model completeness:                  38%
Provider/Capability completeness:           40%
Multi-language service layer completeness: 22%
```

这些百分比是工程估计，不是形式化覆盖率。

1. Semantic Contract 的纯 Rust 模型、Proto 和语言中立 TCK 相对完整，九个 durable noun 都有数据模型和主要验证规则。
2. production Kernel 已真实实现 lease allocation、finite TTL、fence、Worker 启停、heartbeat watchdog、Operation 状态、Endpoint 元数据和有界事件分页。
3. production authority 没有实现 Provider 注册、`PublishInventory`、`ReconcileProvider`，ProviderSnapshot 仍只存在于模型/TCK。
4. Namespace 完全缺失，不是 Partial。对象、请求、事件、查询和授权都没有 Namespace。
5. Principal 由可信 `SO_PEERCRED` 注入，这是正确方向；但其逻辑身份直接编码 PID，并以 PID 作为 generation，违反稳定身份规则。
6. Lease/fence 已进入多个真实执行路径，但 lease release 直接 `Active -> Released`；Worker lost 时也未执行 `Lost -> Lease revoke -> fence advance -> event` 闭环。
7. canonical `SubscribeEvents` 是 unary bounded pull，不是 server streaming；有明确 GAP/SOURCE_CHANGED，但事件和序列只在内存中，重启后无法 replay。
8. daemon 重启只恢复 node epoch 和 fence floor，不恢复 Worker、Lease、Operation、Endpoint 或 Event。sandboxd 采用旧 cgroup 清场，而不是 Discover/Classify/Recover。
9. 上层 plugin/service 系统已经有 Rust/Python worker protocol 和 typed proxies，但能力体系是中央 10 类枚举/oneof，不是开放、版本化、schema 驱动的语言无关 Service API。
10. 当前 workspace 测试和三个 Python TCK 均通过，但最关键的 recovery、namespace、production event resume、stale fence end-to-end 测试缺失。JVM 全生命周期测试被明确 `#[ignore]`。

---

## B. Implementation Matrix

| Area | Status | Evidence | Missing / Problem | Priority |
|---|---|---|---|---|
| Identity | 🟡 Partial | `cy-kernel-contract/src/identity.rs`;所有 noun 使用 `Identity{id,generation}` | production Principal 把 PID 写入 ID/generation；部分内部表只按字符串 ID 建索引 | P0 |
| Namespace | ❌ Missing | Semantic model/Proto 无 Namespace；搜索 production 无相关字段 | ObjectRef、request、event、query、authorization 全无隔离 | P0 |
| Generation | 🟡 Partial | semantic models；resource generation、lease generation、Worker checks；node epoch 持久化 | Lease generation 固定为 1；Provider generation/session 不存在；Worker replacement/recovery 不闭环 | P0 |
| Principal | 🔴 Contract Violation | `peer_cred.rs:38-43`; authority interceptor | 可信注入正确，但 PID 被作为稳定语义身份 | P0 |
| Provider | 🟠 Skeleton | `Provider`/`ProviderSnapshot` model；静态 `UdsHardwareAdapterRegistry` | production authority 没有 register/reconcile RPC；无 Provider session generation/TTL | P0 |
| Capability | 🟡 Partial | `CapabilityRequirement::matches`; `ResourceQuery::matches`; lease allocation 使用它 | Kernel 资源匹配真实可用；Provider discovery/disappearance resolution 不完整；上层另有中央枚举 | P1 |
| Resource | 🟡 Partial | adapters 发布 generic `Resource`; resource manager 原子筛选 | inventory aggregate generation 与 Provider snapshot generation 混合；状态仅内存 | P1 |
| Lease | 🔴 Contract Violation | `InMemoryResourceManager`; authority acquire/renew/release | `release()` 直接 `ACTIVE -> RELEASED`；无 holder Principal authorization；无 revoke API | P0 |
| Fence | 🟡 Partial | durable fence floor；start/stop/heartbeat/worker envelope checks | runtime launch adapter不接收 lease/fence；旧 Worker 对资源侧行为缺少统一验证；restart 只保证 token 不复用 | P0 |
| Worker | 🟡 Partial | `StartWorker -> InstanceActor -> sandboxd`; heartbeat/watchdog | 无 restart adoption；timeout 未写 `WorkerLost`；Worker state 删除后不可查询；Provider identity 未验证 | P1 |
| Operation | 🟡 Partial | canonical create/report/cancel；start/stop 返回 Operation | 全内存；start Operation 不随 heartbeat 成功完成；canonical cancel 不驱动 executor cancellation | P1 |
| Endpoint | 🔴 Contract Violation | publish/authorize/revoke 进入 authority RPC | 不验证调用 Principal 是否为 Worker owner；grant 无消费/查询路径；无 lease revoke 联动 | P0 |
| Events | 🟡 Partial | bounded semantic `VecDeque`; source/sequence/kind/time/body | 只内存；部分事件丢弃 validation error；没有 live canonical stream；缺少 lease/resource 状态事件 | P1 |
| Snapshot | ❌ Missing | 只有 `ProviderSnapshot` value model | 没有 Kernel `GetSnapshot(state+cursor)` | P1 |
| Replay | 🟡 Partial | `semantic_events_after` 返回 CURRENT/GAP/SOURCE_CHANGED | 仅当前进程内 256 条；restart 后 source changed；不是 durable at-least-once stream | P1 |
| Reconciliation | 🟡 Partial | adapter polling；resource inventory refresh；旧 cgroup cleanup | 没有 ProviderSnapshot reconcile；Worker/Lease/Operation/Endpoint 未联动收敛 | P0 |
| Recovery | 🟠 Skeleton | `FileRuntimeJournal::recover`; sandboxd owned-root cleanup | 只恢复 epoch/fence floor；无 Discover/Classify/Adopt/MarkLost | P0 |
| Persistence | 🟡 Partial | JSONL journal 持久 node epoch、fence、少量 lifecycle facts | Worker/Lease/Operation/Resource/Event/Endpoint/Provider 均不重建 | P0 |
| Projection | 🔴 Contract Violation | pure contract crate 无 transport dependency；有 convert 模块 | semantic authority 状态和规则直接实现在 tonic adapter；`KernelAuthority` trait 没有 production impl | P0 |
| Service API | 🟠 Skeleton | Plugin Envelope 有 hello/invoke/result/error/cancel/stream | 中央 typed oneof；无开放 Capability ID+version+schema；JVM lifecycle 测试 ignored | P2 |
| Package/Manifest | 🟡 Partial | plugin schema、Rust model、verified `launch.json` resolver | 不是最小 `cy.json`；无 `provides/requires/run` 通用模型；无 `.cyp` 路径 | P2 |

### Semantic Noun Details

| Noun | Model / Identity | Lifecycle owner and storage | Main deficiencies |
|---|---|---|---|
| Principal | `Principal{Identity}` | UDS interceptor derives it; not persisted | PID identity violation；authorization policy mostly absent |
| Provider | `Provider{Identity,state,capabilities}` | No production lifecycle owner | model/TCK only; static configured adapters are not semantic Provider registration |
| Resource | generic class/capability/capacity/attributes/topology | adapter observes; resource manager stores in memory | no durable identity ledger; no complete ProviderSnapshot TTL |
| Lease | `ResourceLease` compatibility model projected to semantic Lease | in-memory resource manager | direct release transition; generation always 1; no revoke/replace |
| Worker | semantic Worker stored inside `ManagedProcess` | authority adapter + `InstanceActor`; memory only | no recovery; lost state not maintained |
| Operation | semantic Operation map | authority adapter; memory only | detached from actual long lifecycle completion/cancel |
| Capability | open string + revision + properties | resources and queries | Kernel matching exists; Provider resolution lifecycle absent |
| Endpoint | endpoint/grant maps | authority adapter; memory only | owner Principal authorization missing; no grant enforcement consumer |
| Event | semantic Event deque | authority adapter; memory only | no persistence/live canonical stream/snapshot consistency |

### Abstraction Leak Check

Kernel semantic model and canonical authority RPC do **not** introduce `Model`, `Dataset`, `Tokenizer`, `Docker`, `CudaStream`, `TrainingJob`, `InferenceServer`, `NvidiaGpu` or `VllmServer` as durable nouns.

Upper-layer `cy-platform-api` does define AI-specific central SPI types, including `ModelAnalyzer`, `TrainingBackend`, `ExecutionEngine` and `Quantization`. This is outside Kernel, so it is not a Kernel abstraction leak, but it is an experimental upper-layer taxonomy and should not be promoted into Kernel semantics.

---

## C. Real Runtime Flow

### Startup

```text
systemd / direct binary
  -> runtime/cyrene-kernel/src/main.rs::main
  -> FileRuntimeJournal::open
  -> begin_epoch(node_id)
       -> recover max node epoch + max historical fence
       -> append KERNEL_STARTED + fsync
  -> UdsSandboxAdapterClient
  -> InMemoryResourceManager(next_fence_floor)
  -> KernelDaemon::with_hardware_adapters
       -> UdsHardwareAdapterRegistry(static configured endpoints)
  -> preflight sandbox + hardware adapters
  -> refresh_inventory_facts
       -> adapter GetInventory
       -> InMemoryResourceManager::refresh_inventory
  -> KernelServiceAdapter
       -> in-memory Worker/Operation/Event/Endpoint maps
  -> start watchdog thread
  -> start adapter polling thread
  -> authority UDS:
       KernelAuthorityService + legacy KernelService
       with SO_PEERCRED Principal interceptor
  -> worker UDS:
       WorkerControlService + legacy lifecycle service
```

### Canonical Request

```text
SDK / Node Agent / local client
  -> gRPC over authority UDS
  -> inject_authority_principal(SO_PEERCRED)
  -> KernelAuthorityService method
  -> validate AuthorityCallContext
  -> transport DTO conversion
  -> KernelServiceAdapter state + KernelDaemon
  -> ResourceManager / InstallationResolver / Sandbox client
```
`KernelAuthorityService` 不是薄 projection。它直接拥有并修改 semantic maps，未通过 production `KernelAuthority` implementation。
### Lease

```text
AcquireLease
  -> ResourceQuery validation/matching
  -> hardware inventory refresh
  -> InMemoryResourceManager::reserve
  -> allocate resource + fence
  -> append LeaseReserved to JSONL
  -> return Lease
```

### Worker

```text
StartWorker
  -> verify active lease holder/generation
  -> create adapter binding
  -> resolve execution_ref via verified launch record
  -> InstanceActor::start
  -> UdsSandboxAdapterClient::launch
  -> sandboxd CgroupV2Runtime::launch
       -> create cgroup
       -> limits/device enforcement
       -> spawn process
       -> attach cgroup
  -> store ManagedProcess in memory
  -> emit worker.starting
  -> return RUNNING Operation
```

### Worker Control / Timeout

```text
WorkerControl Connect
  -> Hello(worker, lease, fence)
  -> check current active lease
  -> heartbeat updates Worker STARTING -> RUNNING
  -> watchdog detects deadline
  -> request shutdown
  -> sandbox stop / cgroup.kill
  -> release lease if cleanup complete
  -> remove ManagedProcess
```

缺失步骤：`Worker -> LOST`、Lease `REVOKED`、fence advance、Operation `LOST` 和 corresponding semantic events。

### Provider Resolution

当前没有 semantic Provider resolution：

```text
static CLI --hardware-adapter ID=/socket
  -> UdsHardwareAdapterRegistry
  -> iterate all configured adapters
  -> aggregate inventory
  -> route create_binding by resource.provider.id
```

这属于静态配置和 resource capability filtering，不是完整 Provider registration/discovery/resolution。

### Event

```text
state change
  -> publish_semantic_event
  -> assign process-local AtomicU64 sequence
  -> append to in-memory VecDeque(capacity=256)

SubscribeEvents(cursor)
  -> unary EventPage
  -> CURRENT / GAP / SOURCE_CHANGED
```

Legacy `WatchOperations` 是 server-streaming，canonical `SubscribeEvents` 不是。

### Shutdown

Kernel 没有显式 graceful shutdown orchestration。两个 tonic server 由 `try_join!` 持续运行，进程信号终止依赖外层/systemd。sandboxd 的 `PDEATHSIG` 和下次启动 owned-cgroup cleanup 提供执行域清场。

---

## D. Contract Violations

### 1. Principal 使用 PID 作为稳定 Identity ✅ 已修复（`peer_cred.rs` 继续保留 `SO_PEERCRED` 的 PID/UID/GID transport evidence，但以稳定的 UID/GID policy subject 映射 `Principal.id`，并固定 `Principal.generation = 1`；同 UID/GID 的不同 PID 已有回归测试证明身份不变。）

**Violation**

外部语义 Principal identity 包含 PID，generation 也是 PID。

**Evidence**
`kernel/crates/cy-kernel-daemon/src/peer_cred.rs:33-43`
```text
id = unix://uid=.../gid=.../pid=...
generation = pid
```

测试在 `kernel/crates/cy-kernel-daemon/src/tests.rs:763-776` 固化该行为。

**Why it violates Contract**

Contract §2 禁止将 PID/native handle 放入 semantic wire contract；Identity 应是 opaque logical identity。PID 可复用，也不是 Principal 的稳定安全主体。

**Impact**

同一 Unix 用户的每个进程成为不同 Principal；PID reuse 可能重用语义 generation；持久 owner/authorization 无法稳定表达。

**Suggested direction**

transport 仍可验证 PID，但 Principal 应映射为稳定 policy identity，例如 UID/GID policy subject、mTLS subject 或 authority-issued opaque Principal ID；PID 只留在 transport evidence。

---

### 2. Lease release 跳过 `RELEASING` 🟡 部分修复（资源账本已拆为 `begin_release -> RELEASING`、`complete_release -> RELEASED` 与 `fail_release -> FAILED`；资源仅在 cleanup 完成后重新可分配，canonical release 在 durable `LEASE_RELEASE_STARTED` 后才进入 `RELEASING`，cleanup/journal 未完成时保持安全状态。legacy stop/watchdog 路径仍需把 release decision 前移到 physical cleanup 之前，故不能标记完成。）

**Violation**

production `release()` 直接把 `Active` 设为 `Released`。

**Evidence**
`kernel/crates/cy-resource-manager/src/lib.rs:348-379`
**Why it violates Contract**

Contract §6 明确要求 `ACTIVE -> RELEASING -> RELEASED`，可以隐藏中间 observation，但不能直接 mutation。

**Impact**

无法表示正在执行资源 cleanup；并发查询会看到资源已释放而执行域可能尚未完成；难以实现 fail-closed recovery。

**Suggested direction**

把 release authority transition 和物理 cleanup completion 分开，只有 cleanup confirmed 后进入 `RELEASED`。

---

### 3. Endpoint authority 不验证调用 Principal ✅ 已修复（`publish_endpoint`、`authorize_endpoint`、`revoke_endpoint` 均从已认证 Principal 比较 Worker owner identity，并验证 Worker incarnation 与 owner Lease 的活动状态；不同 Principal 的三项操作均有拒绝回归测试。）

**Violation**
`publish_endpoint`、`authorize_endpoint`、`revoke_endpoint` 只确认 request 经认证，但不比较 authenticated Principal 与 Worker owner。
**Evidence**
`authority_service.rs:616-731` 中 `_principal` 未用于授权。
**Why it violates Contract**

Contract §8 要求只有 Worker owner Principal 或 Kernel policy 才能 publish/authorize/revoke。

**Impact**

任意能访问 authority socket 的本地认证进程可能给别的 Worker 发布或撤销 Endpoint grant。

**Suggested direction**

保存并检查 Worker Principal ownership；Kernel policy override 必须显式、可审计。

---

### 4. Semantic state machine 位于 transport adapter ⬜ 未修复（本轮没有在 Frozen v1 之外重写 authority port；production `KernelAuthority` implementation 与 projection 完整分离仍是 P0 blocker，不能将已修复的 Endpoint/Lease 局部逻辑误报为完成。）

**Violation**
`KernelServiceAdapter` 同时是 tonic service、semantic store、Operation manager、Endpoint authority 和 Event store。
**Evidence**
`adapter/mod.rs:34-52`; `rpc/authority_service.rs`

`KernelAuthority` trait 位于 `cy-kernel-api/src/authority.rs`，但没有 production implementation。
**Why it violates Contract**

Contract 将 gRPC/Proto 定义为 projection。当前 production semantics 依赖 tonic request/status 和 generated Core DTO。

**Impact**

替换 transport 会复制或重写 domain behavior；不同 projection 可能出现不同授权和 transition 规则。

**Suggested direction**

让 transport 只完成 authentication、mapping、error projection；状态转换由 transport-independent `KernelAuthority` implementation 承担。

---

### 5. Provider v1 stable actions 被 canonical projection 省略

**Violation**

canonical service 没有 `RegisterProvider`、`PublishInventory`、`ReconcileProvider`。

**Evidence**
`contracts/proto/cyrene/core/v1/kernel_authority.proto:15-40`  

`KernelAuthority` trait 声明这些动作，但 gRPC service 未投影。
**Why it violates Contract**

Contract §11 把这些列为 v1 stable actions，transport 可以组合但不能省略其验证和 authority semantics。

**Impact**

Provider session generation、snapshot TTL 和 missing Worker reconciliation 无法从 canonical path 表达。

**Suggested direction**

补齐 canonical Provider actions，或定义一个明确合并入口并证明三类语义独立执行。

---

### 6. 静态 adapter 聚合混淆 Provider incarnation 与 inventory generation

**Violation**

production adapter registry 没有 Provider session generation；通过 inventory fingerprint 产生聚合 generation，并覆盖 `resource.provider.id`。

**Evidence**
`cy-adapter-client/src/registry.rs:90-165`

**Why it violates Contract**

Provider generation、ProviderSnapshot generation 和 Resource generation 是三个不同概念。当前只有 aggregate inventory generation。

**Impact**

adapter restart、same-content reconnect、resource continuity 无法按 Contract 区分；Provider disappearance 只能成为整组 probe error。

**Suggested direction**

独立记录每个 authenticated Provider session generation、snapshot generation 和 Resource continuity generation。

---

## E. Skeletons Mistaken for Implementations

1. `KernelAuthority` trait  
   位置：`kernel/crates/cy-kernel-api/src/authority.rs`。  
   事实：无 production `impl KernelAuthority`；tonic adapter 直接实现行为。

2. `Provider` / `ProviderSnapshot`  
   位置：`cy-kernel-contract/src/resource.rs`、`snapshot.rs`。  
   事实：有模型、验证和 TCK，没有 production Provider registry/reconcile path。

3. `V1_ACTIONS` 的 17 个动作  
   位置：`snapshot.rs:135-153`。  
   事实：只是 enum/list；canonical service 缺 3 个 Provider 动作。

4. `SubscribeEvents`  
   位置：`authority_service.rs:734-753`。  
   事实：是真实分页 API，但不是 streaming；历史只有内存 256 条。

5. `FileRuntimeJournal`  
   位置：`runtime_journal.rs`。  
   事实：是持久 fence evidence，不是 Worker/Lease/Event recovery database。

6. `ProcessHandle.start_time_ticks`  
   位置：`cy-kernel-api/src/runtime.rs`。  
   事实：launch 时采集，但 stop path 没有验证该字段；PID reuse 防护主要依赖 sandboxd 内存 Child/pidfd。

7. `EndpointGrant::authorizes`  
   位置：contract pure model。  
   事实：production authority 创建 grant 时重复部分检查，但没有 data-plane consumer 使用该方法验证访问。

8. `InstanceActor` restart/crash fields  
   位置：`watchdog/instance_actor.rs`。  
   事实：有 crash count、generation、restart helper，但 daemon watchdog 实际路径是 stop/release/remove，不执行 restart policy。

9. `RestartPolicy` manifest  
   位置：`cy-manifest/src/manifest/plugin.rs`。  
   事实：被解析，但 production Worker lifecycle 没有读取执行它。

10. JVM control plane  
    位置：`framework/jvm/bootstrap/...`; `KernelOutboundAdapter.kt`。  
    事实：入口只打印初始化；adapter 返回模拟成功。

11. JVM plugin integration  
    位置：`framework/crates/cy-extension-registry/tests/jvm_integration_test.rs`。  
    事实：`#[ignore]`，主体只是 TODO/print。

12. Service manifests  
    位置：`service.schema.json`, `advanced-service.schema.json`。  
    事实：有 schema 和样例，没有统一 service runtime/discovery/invoke lifecycle。

13. Snapshot + Stream  
    事实：有 `ProviderSnapshot` 和 EventPage 两套独立模型，没有 Kernel state snapshot + cursor 一致性 API。

14. Namespace  
    事实：名称中出现 “namespaced identifier” 只表示字符串 grammar，不是 tenant Namespace。

---

## F. Missing Reliability Paths

| Failure path | Current behavior | Status |
|---|---|---|
| Worker process crash | heartbeat timeout 后尝试 stop/release/remove | 🟡 没有 `WorkerLost`/Lease revoke/fence advance |
| Worker timeout | bounded shutdown + cgroup cleanup | 🟡 semantic lifecycle 联动不完整 |
| Canonical Operation cancel | 只把 Operation 设为 `CANCELLING` | 🟡 不通知 executor，不保证终止 |
| Legacy Operation cancel | 真正 stop/reap/release | 🟡 但与 canonical Operation 是两套模型 |
| daemon crash | 所有 in-memory state 丢失；node epoch/fence floor 恢复 | 🟠 |
| sandboxd crash | Worker 因 `PDEATHSIG` 应终止；Kernel 后续 stop 请求可能失败 | 🟡 未显式 reconcile |
| daemon restart | 新空账本；重新 probe resources | 🟠 旧 leases/workers/events 不重建 |
| provider disconnect | aggregate probe 整体失败，adapter_available=false | 🟡 非按 Provider 隔离 |
| provider reconnect | poll 成功后发 recovered event | 🟡 无 Provider generation/session validation |
| stale process | sandboxd 启动按 owned cgroup 命名清场 | 🟡 无 valid/stale/unknown/foreign 分类 |
| foreign process | 非 owned cgroup 不清理 | ✅ 路径方向安全 |
| PID reuse | live sandboxd child 用 pidfd；handle 有 start ticks | 🟡 restart 后不做身份分类 |
| event disconnect | 客户端可再次 pull cursor | 🟡 仅同一 daemon 进程 |
| event cursor expiry | 返回 GAP | ✅ 对当前内存窗口明确 |
| event source restart | 返回 SOURCE_CHANGED | ✅ |
| slow canonical subscriber | 无 live stream，只是 unary page | ⚪ 不适用，但不满足 streaming 目标 |
| slow legacy subscriber | bounded broadcast/mpsc，lag 后断开 | ✅ |
| Lease expiry | 下一次账本调用时惰性回收 | 🟡 无主动 timer/event/Worker termination |
| Endpoint after lease expiry | grant map仍在；没有消费时实时验证 | 🟡 |
| journal failure on acquire/release | fail-closed 或 rollback | ✅ |
| journal failure on Worker launch/termination | 仅 `eprintln!`，继续运行 | 🟡 recovery evidence 可缺失 |

---

## G. Test Gap

### Existing Coverage Classification

| Golden Test | Status | Evidence |
|---|---|---|
| New Hardware Test | 🟡 Partial | dual adapter UDS integration and generic query pass；仍需 CLI/config entry，且没有 semantic Provider registration |
| Stale Fence Test | 🟡 Partial | resource manager、InstanceActor、Rust/Python worker SDK 有 unit/TCK；无 old Worker recovery end-to-end |
| Event Resume Test | 🟡 Partial | EventPage replay model和 legacy watch replay；无 daemon disconnect/reconnect production test |
| Cursor Expiry Test | 🟡 Partial | pure replay TCK有 GAP；缺 production eviction RPC integration |
| Slow Consumer Test | 🟡 Partial | legacy broadcast lag 和 plugin stream backpressure；canonical event API不是 stream |
| Namespace Collision Test | ❌ Missing | Namespace 不存在 |
| Namespace Isolation Test | ❌ Missing | Namespace 不存在 |
| Restart Recovery Test | 🟡 Partial | fence floor/epoch test；明确“不恢复实例” |
| Transport Replacement Test | 🟡 Partial | pure contract crate independent；production semantics 仍在 tonic adapter |

### Highest-Priority Missing Tests

1. **End-to-end stale Worker fence test**  
   Worker A lease失效，新 Worker/Lease获得新 fence，旧 Worker A 恢复并尝试 heartbeat、invoke、endpoint/data access，所有路径必须拒绝。

2. **Crash while Workers run**  
   启动真实 daemon+sandboxd+Worker，kill daemon，重启，验证 old process classification、lease authority、event/source change 和资源是否可安全再分配。

3. **Worker lost reconciliation**  
   杀 Worker，不走正常 stop，验证 `Worker LOST -> Lease REVOKED -> fence advance -> events -> resource availability`。

4. **Provider loss isolation**  
   两个 Provider 运行，断开一个；另一个仍可分配，丢失 Provider 的新 lease 被拒绝，已有 Worker按政策处理。

5. **Production event eviction/resume**  
   经 authority UDS 生成超过 256 个 semantic events，验证 CURRENT/GAP/SOURCE_CHANGED 和无静默跳转。

6. **Snapshot/cursor atomicity**  
   当前无法编写，因为 API 缺失。实现后验证 snapshot 包含 cursor 前所有 effects。

7. **Endpoint ownership authorization**  
   两个不同 Principal，通过真实 `SO_PEERCRED`/mTLS，证明不能 publish/authorize/revoke 对方 Worker Endpoint。

8. **Namespace collision/isolation**  
   当前无法编写，因为模型缺失。

9. **Lease release transition under incomplete cleanup**  
   验证 cleanup 未完成时不能发布 `RELEASED` 或重新分配资源。

10. **JVM/Rust replacement test**  
    同一 capability/schema，分别由 JVM 与 Rust Worker 实现，消费者代码和 wire request 不变。当前 JVM test ignored。

### Verification Performed

- IDE run configuration `Test Cyrene-Platform`: exit code `0`.
- Python Kernel Semantic TCK v1: passed.
- Python Worker-control TCK v1: passed.
- AstrBot static transition vectors: 5 passed.
- JVM full lifecycle test: ignored，缺 runnable JVM installation/sandboxd fixture。
- Multi-account UDS acceptance tests: ignored，需要 root 和第二本地账号。
- 测试后 git status 与审计前一致，无代码修改。

---

## H. P0 / P1 / P2 Development Plan

### P0 - 阻止错误架构继续固化

1. 去除 Principal 对 PID 的语义身份依赖；PID 只作为 transport evidence。
2. 明确 Namespace 是否属于当前 Contract 演进。如果这是当前 Semantic Contract 的强制要求，应先补模型、引用、请求、事件、存储键和 authorization，再继续扩展对象。
3. 建立 transport-independent production `KernelAuthority` implementation，避免继续在 tonic adapter 中增加状态和规则。
4. 修正 Lease release 状态机，拆分 authority release、physical cleanup、terminal released。
5. 补齐 Endpoint Principal ownership authorization。
6. 补齐 Provider registration/session/snapshot/reconcile production path，分离 Provider、snapshot、Resource generation。
7. 定义 Worker lost 与 Lease revoke/fence advance 的原子联动。
8. 防止 runtime journal 的非关键写失败被静默降级；确定哪些 transition 必须 durable-before-visible。
9. 统一 canonical 与 legacy Operation/Events 迁移边界，避免两套不一致语义长期共存。

### P1 - 完成 Kernel v1 语义闭环

1. 持久化或可安全重建 Provider、Resource、Lease、Worker、Operation、Endpoint 和 Event source/cursor 所需状态。
2. 实现 ProviderSnapshot TTL 和 per-Provider reconciliation。
3. 实现 Worker `Registered -> Starting -> Running -> Lost/Stopped/Failed` 的可查询状态，而不是终态直接删除。
4. 实现 Operation 与实际 executor lifecycle/cancel 的连接。
5. 实现 lease revoke/replace 和 monotonic fence durable reservation。
6. 实现 canonical ordered replayable event log，明确 at-least-once 和 retention。
7. 实现 Kernel snapshot + cursor 一致性接口。
8. 实现 Discover/Classify/Recover，至少区分 valid/stale/unknown/foreign。
9. 将 sandboxd owned-cgroup cleanup 纳入显式 recovery result，而不是启动副作用。
10. 补齐上述 P0/P1 integration 与 failure injection 测试。

### P2 - 上层能力

1. 将 service capability 从中央 10 类 enum/oneof 演进为开放 ID + version + input/output/error/stream/cancel schema。
2. 构建语言无关的 `Invoke/Result/Error/Event/Cancel/Health` Service API。
3. 让 Java/Kotlin/Rust/Python/Node/C# 只是 projection，运行时语言不进入消费者接口。
4. 定义最小 `cy.json`：identity、version、run、provides、requires。
5. 若需要 `.cyp`，保持其为普通目录 manifest 的可选归档格式。
6. 完成真实 JVM Worker fixture 和跨语言替换测试。
7. 再逐步接入 AI services、Training、Deployment、Desktop integration；不要提升为 Kernel nouns。

---

## Source Organization

主要超过阈值的手写源码：

| File | Lines | Assessment |
|---|---:|---|
| `kernel/crates/cy-kernel-daemon/src/tests.rs` | 1461 | 测试 God File，影响导航但非运行风险 |
| `adapters/execution/sandboxd/src/runtime.rs` | 987 | 未超过 1200；承担 cgroup、process、transport bridge，多责任但仍是一个 concrete adapter |
| `sdk/python/.../cyrene_worker.py` | 824 | 手写 protobuf + SDK loop，维护风险较高 |
| `watchdog/instance_actor.rs` | 818 | lifecycle、transport multiplexing、timeout/restart 混合 |
| `rpc/authority_service.rs` | 754 | 最大语义 God File 风险，问题不在行数，而在 transport 与 domain state 混合 |

没有 `>1800` 或 `>3000` 的单个主要生产源码。真正的 God Object 是 `KernelServiceAdapter`，它跨文件同时持有 Worker、Operation、Event、Endpoint、legacy projection 和 watchdog state。

---

## Premature Complexity

- 没有发现完整分布式 scheduler、service mesh、自研 container runtime、数据库或 IAM 平台。
- sandboxd/cgroup/BPF 实现是当前本地资源隔离和 cleanup 所需，不属于明显过早复杂化。
- 上层 10 类 plugin taxonomy、每类 Proto message 和 proxy 较重。当前真实 consumer 很少，JVM integration 仍 ignored，因此有一定 premature rigidity 风险。
- `cy-manifest` 已包含大量 model/training/runtime/package taxonomy，但它位于上层 contracts，不是 Kernel；在真实 service lifecycle 尚未闭环前继续扩张该词汇的收益有限。
- `.cyp`、dependency solver、复杂 package manager 尚未实现，这符合“不要提前实现”。

---

# Final Questions

## Question 1

如果现在接入一个全新的 GPU/NPU Provider：

```text
Kernel semantic core 是否需要修改？
```

**对资源模型和查询算法本身，理论上不需要。对当前 production integration，通常需要配置和可能的 adapter protocol实现，但不应修改 semantic core。**

当前 generic 部分已经支持：

```text
Resource.class
Capability{id, revision, properties}
capacity
attributes
topology
ResourceQuery exact matching
```

一个新 Provider 可以实现现有 hardware adapter protocol，发布 opaque class/capabilities/resources，然后通过：

```text
--hardware-adapter ID=/socket
```

接入。双 adapter integration 已证明静态聚合和 provenance routing 工作。

但当前仍有以下限制：

1. Provider 不是动态 semantic registration，只是启动 CLI 静态配置。
2. 没有 Provider generation/session、snapshot TTL 或 per-Provider reconcile。
3. adapter protocol 只有 inventory 和 create binding；若 NPU 需要现有 `DeviceBinding` 无法表达的执行授权，可能迫使修改 compatibility port。
4. 整个 aggregate probe 对单 Provider 失败不隔离。
5. 新 Provider 不能通过 capability discovery 自动出现/消失。

结论：

```text
Semantic nouns/query: 不应修改
Production Kernel wiring/config: 需要
若现有 DeviceBinding 不足: 当前实现会诱发 Kernel port 修改
```

成熟度判断：**New Hardware Test 仅 Partial。**

---

## Question 2

如果把已有 Java Service 完全重写成 Rust，保持 Capability 和 Contract 不变：

```text
消费者是否需要修改？
```

**当前不能保证不修改。**

理想路径下，消费者只看 capability/version/schema，语言替换应透明。但当前实现存在以下泄漏：

1. `PluginManifest.runtime` 显式区分 `subprocess-jvm` 和 `subprocess-python`，甚至没有通用 native/Rust runtime variant。
2. manifest 包含 JVM `entrypoint` class name。
3. `PluginKind` 是中央枚举，注册表和 proxies 按 Rust trait 类型分组。
4. protocol `Invoke` 是中央 oneof，写死 `ExecuteInferenceRequest`、`RunTrainingStepRequest` 等。
5. capability handshake 主要是字符串列表和 `capabilities_json`，没有统一版本和 input/output schema contract。
6. JVM full lifecycle test ignored，无法证明 JVM/Rust replacement。
7. service schema 没有通用 `run/provides/requires`，也没有 production service runtime。

如果消费者直接调用现有 typed SPI，重写语言只要 Rust Worker 完全实现同一 Protobuf oneof，消费者源代码可能不改。但 installation manifest、runtime adapter、packaging 和可能的 registry registration 都要改，且仓库没有端到端证据。

成熟度判断：**语言替换透明性尚未成立。**

---

## Question 3

如果 daemon 在所有 Worker 运行时突然 crash，然后重新启动：

```text
系统是否能够安全地重新建立现实状态？
```

**不能完整恢复语义状态。它主要依赖执行域清场和 fence floor 前进来避免旧 authority 被复用。**

当前实际步骤：

1. Kernel 进程 crash。
2. `KernelServiceAdapter` 中的 Worker、Operation、Event、Endpoint、grant maps 全部丢失。
3. `InMemoryResourceManager` 中 Resource allocations 和 Lease records 全部丢失。
4. 如果 sandboxd 仍运行，Worker 可能暂时继续运行；Kernel 已没有它们的 handle/ownership record。
5. 如果 sandboxd 与 Kernel 同时死亡，Worker 通过 sandboxd 设置的 `PR_SET_PDEATHSIG=SIGKILL` 应被杀。
6. sandboxd 下次启动执行 `initialize_owned_root()`，扫描安全命名的 `instance-*` cgroup 并 `cgroup.kill`。它不会 kill 非 owned/foreign cgroup。
7. Kernel 重启读取 JSONL：
   - 恢复历史最大 node epoch；
   - 恢复历史最大 fence token + 1；
   - 不 rehydrate Worker、Lease 或 ProcessHandle。
8. Kernel 写入新 epoch，然后创建全新的空 resource manager。
9. Kernel 重新从静态 hardware adapters probe inventory。
10. 所有资源看起来未分配，因为旧 Lease ledger 未恢复。
11. 新 Lease 使用高于历史值的 fence，因此旧 fence 不会与新 lease 相等。
12. 旧 Event cursor 因 node epoch 改变得到 `SOURCE_CHANGED`，但没有 snapshot API帮助客户端重建。
13. 旧 Worker若侥幸仍存活：
   - canonical heartbeat/control 连接会因为新 Kernel没有 Worker/Lease record而被拒绝；
   - 但系统不会把它分类为 `stale/unknown/foreign`，不会生成 WorkerLost/LeaseRevoked 事件；
   - 是否已被彻底阻止访问物理资源依赖 sandboxd/cgroup cleanup，而不是 Kernel recovery ledger。
14. 新 Kernel可能在确认旧执行域完全清理前重新分配资源，尤其当 sandboxd未重启或 cleanup状态无法证明时，缺少统一的启动 gate。

因此当前不是：

```text
Discover -> Classify -> Recover
```

而更接近：

```text
Advance epoch/fence
-> forget semantic state
-> rely on sandboxd/PDEATHSIG cleanup
-> rebuild inventory as unallocated
```

它能降低 stale fence reuse 风险，也避免随意 kill foreign process，但不能证明安全地重新建立全部现实状态。

**最终成熟度判断：Recovery 为 Skeleton/Partial，尚不满足 Contract 的完整安全恢复闭环。**
