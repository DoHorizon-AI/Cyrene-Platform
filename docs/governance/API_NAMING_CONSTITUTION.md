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

Canonical semantic operations preserve the full semantic noun across language
boundaries. The spelling changes only with the language convention:

```text
AcquireLease -> acquire_lease -> acquireLease
RenewLease   -> renew_lease   -> renewLease
ReleaseLease -> release_lease -> releaseLease
```

Contextual abbreviations such as `acquire()` are not used for canonical Lease
operations. A receiver type may make a shortened method acceptable for a
different, unambiguous domain operation, but it does not authorize shortening
the canonical Lease vocabulary.

### 6.1 Envelope and type responsibility

A DTO or envelope has one primary semantic responsibility. Event contracts use
the following separation:

```text
Event                immutable event record
EventCursor          durable position and source identity
EventPage            finite ReadEvents history result
EventContinuity      typed history/stream continuity condition
WatchEventsResponse  one item/control frame in continuous observation
```

`EventPage` is never used as a `WatchEvents` stream control payload.
`WatchEventsResponse` carries either an `Event` or the dedicated
`EventContinuity` frame. `EventContinuity` reuses the canonical
`ReplayStatus` values (`CURRENT`, `GAP`, `SOURCE_CHANGED`) and does not create
a second continuity status vocabulary.

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
resource port uses the canonical `acquire_lease` spelling. Qualified Lease and
Event-shape checks are maintained in the machine-readable policy so unrelated
uses of generic verbs remain valid.

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
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene API 命名宪法

**状态：首个 public release 之前新建及迁移的 API 均须遵守本规范。**

本文档是 Cyrene 的规范命名词汇表。Platform 拥有通用词汇；Yield、Reactor、Exchange、Catalyst、Echo、Navigator 和 Plugins 均应消费这些词汇。只有在领域差异真实存在且已有文档说明时，才可增加领域术语。

## 1. 宪法原则

这些规则关乎语义，而非外观：

1. 每个概念只有一个首选词。
2. 每个词只有一种含义。
3. 名称必须标出所属抽象层：逻辑生命周期、具体执行、资源所有权、观测或控制面策略。
4. 首个 public API 冻结之前允许 breaking rename。兼容名称必须明确标记并隔离，不能成为第二个 authority。
5. 首次 public release 开启命名冻结：**Cyrene public API naming freeze begins with the first public release.**

## 2. 规范 action 词汇

| 概念 | 规范用词 | 精确含义 | 使用示例 | 此含义下应避免 |
|---|---|---|---|---|
| 获取 Lease | Acquire | 取得带 ownership/lease 语义的 resource。 | AcquireLease、acquire_lease | allocation 别名或 claim 类动词 |
| 延长 Lease | Renew | 延长现有 Lease 的有效期。 | RenewLease、renew_lease | refresh_lease、extend_lease |
| 持有人释放 | Release | holder 主动放弃 Lease。 | ReleaseLease、release_lease | free_resources、return_lease |
| 控制面强制收回 | Revoke | authority 取消仍有效的 Lease 或 capability。 | RevokeLease、REVOKED | 用 Release 表示强制收回 |
| 创建逻辑对象 | Create | 创建具有 identity 和 lifecycle 的 entity。 | CreateOperation、CreateWorker | 任意混用 new、make、provision |
| 删除逻辑对象 | Delete | 删除持久或逻辑对象。 | DeleteWorkspace | remove 与 delete 混用 |
| 启动逻辑 entity | Start | 将受管理 entity 转入运行生命周期。 | StartWorker、StartService、StartPlugin | 对逻辑对象使用进程启动动词 |
| 启动具体执行 | Launch | 创建 process、execution 或 runtime instance。 | LaunchProcess、ProcessRuntime::launch | 对 fork/exec 使用 Start |
| 停止逻辑 entity | Stop | 请求受管理 entity 有序停止。 | StopWorker、StopService、StopPlugin | 对逻辑对象使用进程终止动词 |
| 终止具体执行 | Terminate | 结束具体 process 或 execution。 | TerminateProcess、terminate() | 混用 StopProcess 和 TerminateProcess |
| 最终资源清理 | Cleanup | 执行结束后清除残余资源。 | cleanup_execution | 随意使用 delete、remove、purge |
| 单对象查询 | Get | 按稳定 identity 读取已知对象或 snapshot。 | GetWorker、GetLease | 混用 Fetch、Read、Query |
| 有限历史查询 | Read | 从 cursor 开始读取有界的持久历史页。 | ReadEvents | subscription 或 fetch 类别名 |
| 集合枚举 | List | 返回对象集合。 | ListWorkers | GetWorkers、EnumerateWorkers |
| 长期观测 | Watch | 通过 server stream 或长期连接观测变化。 | WatchEvents、WatchOperations | 除非确实创建 subscription，否则不用 subscription 动词 |
| 持久订阅 | Subscribe | 创建具有 cursor/delivery 语义的订阅。 | 确实存在该资源时用 Subscribe | 把普通 watch 称为 subscription |
| Agent/Provider 事实提交 | Report | 向其 authority 提交观测事实。 | ReportHeartbeat、ReportOperation | 用 heartbeat 命名 authority report |
| Identity 注册 | Register | 将既有 identity 加入 control plane。 | RegisterWorker、RegisterProvider | 用 Create 表示注册 |
| 取消未完成 operation | Cancel | 请求 operation 不再继续。 | CancelOperation | 随意使用 StopOperation、AbortOperation |
| 强制内部中断 | Abort | 在不安全/错误路径立即中断执行。 | 内部 abort_execution | 普通用户取消 |
| 执行准备 | Prepare | 在 launch 前准备工作。 | PrepareExecution | 混用 Initialize、Setup |
| 健康检查/读取 | Check / GetHealth | 主动检查或读取健康状态。 | health_check()、GetHealth | 将 Ping 作为健康契约 |
| 外部事实刷新 | Refresh | 重新读取外部世界中的事实。 | RefreshInventory | 用于刷新 Lease TTL |

Start 与 Launch 有意区分：Start 改变受管理逻辑 entity 的 lifecycle；Launch 创建具体 runtime instance。同样，Stop 和 Terminate 不同；Cleanup 是 termination 之后的资源阶段。

## 3. 规范概念词汇

| 概念 | 规范含义 | 不要替换为 |
|---|---|---|
| State | 一个 lifecycle state machine 当前所在位置。 | Status、Phase |
| Status | 由多项事实汇总出的当前结果。 | State |
| Phase | 一个 operation 内部的步骤。 | State |
| Event | 对已发生事实或状态变化的不可变记录。 | Notification |
| Fact | 对外部世界的客观观测。 | State |
| Spec | 对期望对象或执行的声明式描述。 | Config |
| Config | daemon、client 或 runtime component 自身的配置。 | Spec |
| Policy | 对允许、拒绝或限制事项的治理约束。 | Config |
| Options | 单次 invocation 的非核心可选修饰项。 | Config |
| Metadata | 描述性 label 和 annotation。 | Spec |
| Id | 持久的逻辑 identity。 | Key、Handle |
| Key | map 或 database 的 lookup key。 | Id |
| Handle | 对活动 runtime object 的控制引用。 | Id |
| Ref | 用来定位另一对象的引用。 | Handle |
| Token | fencing、authentication 或 capability token。 | Id |
| Error | API call 失败。 | Reason |
| Reason | 当前 state/status 是因何进入。 | Error |
| Cause | 形成 error 或 reason 的底层原因。 | Reason |

例如，WorkerState、ServiceState 与 OperationPhase 可以同时成立，因为它们指向不同层级的 state machine。若 WorkerStatus 只是重复 Worker lifecycle state，就不得引入。

## 4. Platform entity 词汇

| Entity | 唯一含义 |
|---|---|
| Principal | 具有 identity 与 authorization 语义的主体。 |
| Provider | 提供 resource 或 capability 的实现。 |
| Resource | 可被发现、分配并租用的 capability/entity。 |
| Lease | 对 Resource 的限时控制权。 |
| Worker | 由 Platform 管理的执行主体。 |
| Operation | 带有 lifecycle、可观测且可取消的工作请求。 |
| Capability | 主体或 Provider 能够提供或执行的能力。 |
| Endpoint | 正在运行的 service 可被访问的地址。 |
| Event | 不可变的变化记录。 |
| Binding | resource 与 execution/consumer 之间的具体关联。 |
| Execution | 一个具体 runtime instance。 |
| Process | OS 级进程。 |
| Service | 长期运行、受管理的 service。 |

以下区分是强制要求：

- Worker 不等于 Process
- Operation 不等于 Execution
- Resource 不等于 Device
- Lease 不等于 Allocation

Adapter 可以把 Device 暴露为一种 Resource，但两个词不能互换。Allocation 是结果或 binding 细节；Lease 是约束它的限时 authority。

## 5. Protobuf 与 RPC 规则

### 5.1 名称

Public RPC 使用 VerbNoun；request 和 response wrapper 与 RPC 使用完全相同的名称，例如：

- AcquireLease 使用 AcquireLeaseRequest 和 AcquireLeaseResponse；
- ReportHeartbeat 使用 ReportHeartbeatRequest 和 ReportHeartbeatResponse；
- ReadEvents 使用 ReadEventsRequest 和 ReadEventsResponse；
- WatchEvents 使用 WatchEventsRequest，并返回 WatchEventsResponse stream。

不要为了绕过 symbol collision 而在 request 前添加 Semantic、Core、Sandbox、Hardware 或其他 adapter prefix。应通过选择 owner package、抽取共享 request，或显式命名 compatibility projection 来解决。不得在新 contract 中继续扩展或复制带 collision prefix 的 semantic request name；machine-readable forbidden-name list 是执行依据。

### 5.2 Package 拥有领域

Package 拥有领域，type name 描述概念。例如，cyrene.sandbox.v1 package 中可使用 LaunchRequest、ExecutionLimits 和 ProcessHandle。避免在同一 package 冗余命名为 SandboxLaunchRequest、SandboxLimits 和 SandboxProcessHandle；package 已经表达领域边界。

### 5.3 Wire 演进

重命名 Protobuf message 或 RPC 是 breaking contract change。首次 public release 之前，应在一次协调变更中迁移所有已跟踪的 contract input、generated binding、fixture、descriptor、adapter 和 consumer。发布后应使用有版本的 contract 或明确隔离的 compatibility package；绝不能静默重新指派 wire symbol 或 field number。

## 6. Rust 与 SDK 规则

跨语言 API 直接映射，不通过同义词转换：

| Proto | Rust |
|---|---|
| AcquireLease | acquire_lease() |
| RenewLease | renew_lease() |
| ReleaseLease | release_lease() |
| StartWorker | start_worker() |
| StopWorker | stop_worker() |
| ReportHeartbeat | report_heartbeat() |
| ReadEvents | read_events() |
| WatchEvents | watch_events() |

Resource-manager 的 allocation 别名不是规范 Lease operation 可接受的 Rust 拼法。语义重命名必须与 Vec::reserve 等无关方法区分；使用 IDEA/RustRover symbol refactoring 或 compiler 引导的修改，绝不能盲目全仓替换。

规范 semantic operation 必须跨语言保留完整 semantic noun。拼写只随语言惯例变化：

| Proto | Rust | JavaScript |
|---|---|---|
| AcquireLease | acquire_lease | acquireLease |
| RenewLease | renew_lease | renewLease |
| ReleaseLease | release_lease | releaseLease |

规范 Lease operation 不使用 acquire() 等依赖上下文的缩写。Receiver type 可以让另一个含义明确的领域 operation 使用缩写，但这不允许缩短规范 Lease 词汇。

### 6.1 Envelope 与 type 的职责

DTO 或 envelope 只有一个主要语义职责。Event contract 应作如下区分：

| Type | 职责 |
|---|---|
| Event | 不可变 Event 记录 |
| EventCursor | 持久位置与 source identity |
| EventPage | 有限 ReadEvents history result |
| EventContinuity | typed history/stream continuity condition |
| WatchEventsResponse | 连续 observation 中的单个 item/control frame |

EventPage 绝不用作 WatchEvents stream control payload。WatchEventsResponse 携带 Event 或专门的 EventContinuity frame。EventContinuity 复用规范 ReplayStatus 值 CURRENT、GAP、SOURCE_CHANGED，不另建第二套 continuity status 词汇。

## 7. State machine 规则

**一个规范 entity 只有一个规范 lifecycle state machine。**

发现两个同名 type 时，不能只做 rename。在合并或删除 state type 前，逐项追踪：

1. 每个 state 和 transition；
2. 每个 caller 与 adapter boundary；
3. persistence、replay 和 serialization/deserialization；
4. terminal-state 与 recovery 行为；
5. 断言该 state machine 的 test 和 fixture。

若 model 完全相同，则合并到规范 type。若不同，则重命名非规范 model，显露其领域或 compatibility boundary。针对当前 Platform inventory，semantic cyrene.semantic.v1.LeaseState 是规范 Lease lifecycle；旧的 cyrene.core.v1.ResourceLease projection payload 不同，并有 ATTACHED compatibility state，因此在调用方和 persistence 退役之前必须作为独立迁移面处理。不能将它静默合并到 semantic type。

## 8. 重构流程

Breaking migration 必须按以下顺序进行：

1. 阅读本宪法并建立规范词汇表。
2. 盘点 Platform、Products 和 Plugins 中的 public/internal name。
3. 将每个命中项归类为真正同义词、有意区分、legacy compatibility 或重复 model。
4. 在可用时使用 IDEA/RustRover 的 Rename Symbol、Find Usages、Safe Delete、Change Signature 和 Move Symbol。
5. 先规范化 Protobuf input；随后重新生成所有 binding 和 descriptor。
6. 先对 Rust 进行 semantic refactoring，再迁移 Python、JVM 和 SDK consumer。
7. 清理 adapter protocol、state-machine ownership、test、fixture 和文档。
8. 运行 compiler/build check、针对性 test、相关完整 test suite 和 forbidden-vocabulary gate。

当前 IDE integration 提供 semantic symbol lookup 和 rename action。Generated binding 及 descriptor/fixture reference 仍需仓库范围 contract search 与重新生成；IDE 的查找结果本身不能证明 wire contract 已全部迁移。

## 9. Forbidden vocabulary gate

机器可读 policy 位于 tooling/architecture/api-naming.toml，checker 位于 tooling/ci/check-api-naming.py。

CI 已针对配置的 source root 以 all_source mode 运行，并拒绝所有已淘汰词汇。Documentation、changelog、migration note 和明确保留的 compatibility fixture 除外。被跳过或未运行的检查不能算通过。

完整 forbidden symbol list 只在 tooling/architecture/api-naming.toml 中维护，作为 machine-readable regression guard。当前 API、comment、example 和治理正文应使用规范词汇，而不是重复已淘汰拼法。由于 Vec::reserve 等无关 API 必须保持有效，reserve 或 start 等通用词不会通过文本匹配直接拒绝；Platform resource port 使用规范拼法 acquire_lease。Qualified Lease 与 Event-shape 检查由 machine-readable policy 维护，使无关的通用动词继续有效。

## 10. Product 与 Plugin 仓库采用规则

Yield、Reactor、Exchange、Catalyst、Echo、Navigator 和 Plugins 必须：

- 在 README 或 developer guide 中链接本宪法；
- 在每个共享 seam 和跨仓示例中使用 Platform 术语；
- 将 Product 自有 state、policy、spec 和 metadata 留在 Product，同时保留这些概念的规范名称；
- 不得为 Acquire/Reserve、Start/Launch、Stop/Terminate 或 Watch/Subscribe 引入本地别名；
- 在本仓 contract 文档中归类任何有意的领域例外，并添加有针对性的 regression test。

Platform 仍是术语的 source of truth。未来可通过共享 vocabulary package 或 TCK 分发本 policy，但必须引用本文档，不能各自重新定义。
