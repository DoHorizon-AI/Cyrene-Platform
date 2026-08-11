# Cy Kernel Contract v1.0
## Development Baseline / Normative Specification

> **状态**：当前开发基线（Normative Development Baseline）  
> **适用范围**：Safe Kernel、daemon、UDS projection、C ABI projection、Stable JVM SPI、Kotlin SDK、Provider/Worker 扩展、持久化与恢复机制  
> **目标**：从本规范生效开始，所有新开发、重构、接口设计和兼容性决策都必须首先服从本规范。  
> **核心原则**：先冻结语义边界，再允许实现自由演进。

---

## 0. 规范关键词

本文使用以下约束词：

- **MUST / 必须**：不可违反。违反即视为 Contract 变更。
- **MUST NOT / 禁止**：明确禁止。
- **SHOULD / 应当**：默认遵循；如偏离，必须有清晰工程理由。
- **MAY / 可以**：实现自由。
- **Semantic Contract**：语义契约，不等于任一具体语言、传输或序列化格式。
- **Projection**：Semantic Contract 在 UDS、C ABI、JVM、Python 等表面接口上的投影。

---

# 1. 系统唯一标准：Semantic Contract

系统真正的标准是：

```text
                    Cy Kernel Semantic Contract
                               │
              ┌────────────────┼────────────────┐
              ↓                ↓                ↓
         UDS Protocol        C ABI         Stable JVM SPI
              │                │                │
             Rust            C/C++          Java/Kotlin
                                                │
                                                ↓
                                           Kotlin SDK
```

以下任何一项都 **不得成为最终标准**：

- Kotlin API
- C ABI
- UDS message layout
- Rust struct
- serialization format
- daemon 内部模块结构
- 某个 Provider 的实现细节

必须始终遵循：

> **先定义语义，再将同一语义投影到不同语言、ABI 和 transport。**

例如语义：

```text
AcquireLease(ResourceQuery)
```

可以分别投影为：

```text
UDS       -> ACQUIRE_LEASE
C ABI     -> cy_lease_acquire(...)
JVM SPI   -> ResourceService.acquire(...)
Kotlin    -> resources.acquire(...)
```

这些只是不同 projection，表达的是同一件事。

---

# 2. Kernel Constitution：内核宪法

后续设计默认遵循以下十条原则：

1. **Kernel owns authority**  
   内核拥有身份、授权、资源所有权和关键状态转换的最终裁决权。

2. **Expose semantics, not implementation**  
   内核暴露 Resource / Lease / Operation / Endpoint 等语义，不暴露 CUDA、PID、fd、pointer 等实现细节。

3. **Every controlled resource has ownership semantics**  
   对受控资源产生影响的行为必须经过 Lease / Fence 或等价权威验证。

4. **External capability enters through Provider**  
   新硬件、新 runtime、新系统能力原则上通过 Provider + Capability 扩展，而不是增加 Kernel 专用概念。

5. **Execution lives in a failure domain**  
   实际执行由 Worker 承担；Worker 是可失败、可丢失、可恢复的执行单元。

6. **All control uses one canonical contract**  
   acquire / start / authorize / connect / reconcile 等控制行为服从同一 Semantic Contract。

7. **Data bypasses Kernel**  
   Kernel 管授权和控制，不成为 tensor、共享内存、RDMA、CUDA IPC 等高带宽数据路径。

8. **Compatibility stays outside the semantic core**  
   兼容性适配、语言 ergonomics、旧协议包装尽量放在 projection / SDK / adapter 层。

9. **State converges through reconciliation**  
   系统不得依赖“所有组件一直在线”。Kernel、Provider、Worker、机器均可能重启或失联。

10. **Events are facts, not callbacks**  
    Event 描述已经发生的事实；Event 不承担命令语义，不建立复杂双向 callback 控制模型。

---

# 3. Kernel Vocabulary：核心词汇固定为约 9 个对象

Kernel 核心只认识：

| 核心对象 | Kernel 必须认识它的原因 |
|---|---|
| `Principal` | 谁在操作 |
| `Worker` | 谁在真正执行 |
| `Provider` | 谁提供能力与资源 |
| `Operation` | 正在进行什么受管理操作 |
| `Resource` | 系统控制什么资源 |
| `Lease` | 谁暂时拥有资源 |
| `Capability` | 能做什么 |
| `Endpoint` | 执行单元如何建立受授权的数据通路 |
| `Event` | 世界发生了什么 |

## 3.1 禁止继续膨胀 Kernel vocabulary

以下概念默认属于上层或 Provider 内部，**不得直接进入 Kernel 核心词汇**：

```text
Model
Dataset
Tokenizer
Docker
PythonEnv
CudaStream
Tensor
LoRA
InferenceServer
TrainingJob
VllmServer
NvidiaGpu
RocmDevice
```

如果未来新增 NPU、训练框架、容器实现或 AI runtime，需要为 Kernel 增加大量专用对象或 RPC，默认判定为**抽象泄漏**。

## 3.2 新增核心名词的门槛

新增第 10 个、第 11 个核心名词之前，必须证明：

1. 它不能由现有 9 个对象组合表达；
2. 它不是 Provider-specific；
3. 它不是 data-plane detail；
4. 它不是某种语言或 transport 的实现细节；
5. 不增加它会造成真正的语义错误，而不仅仅是不方便。

否则不得新增。

---

# 4. Identity / Namespace / Generation：统一身份基础

所有可寻址对象必须建立在 opaque identity 上。

逻辑模型：

```text
ObjectRef {
    namespace
    id
    generation
}
```

其中：

- `namespace`：对象所属的逻辑隔离域；
- `id`：稳定、opaque、不可推导实现细节的逻辑身份；
- `generation`：同一逻辑身份的当前 incarnation / generation。

Kernel 对外 **禁止暴露并依赖**：

```text
PID
fd
pointer
raw socket handle
cuda handle
container id
OS-specific process identity
```

这些信息可以存在于 Provider、Worker launcher、daemon recovery record 等内部实现中，但不能成为 Semantic Contract 的身份基础。

## 4.1 Generation 规则

当逻辑对象发生足以使旧 handle / token / runtime incarnation 失效的替换时：

```text
generation++
```

旧 generation 上的：

- handle
- lease
- fence
- runtime ownership
- endpoint authorization

必须能够被识别为 stale。

`generation` 与 Fence 共同构成 stale actor 防护基础。

---

# 5. Namespace：现在进入模型，隔离能力分阶段实现

Namespace 从 v1 起是**一等语义字段**。

## 5.1 强制规则

- Namespace **MUST** 是独立字段。
- Namespace **MUST NOT** 通过 `"team-a:process-123"` 一类字符串前缀 hack 实现。
- 所有对象引用、事件、授权判断和关键查询都必须能够携带 Namespace。
- 第一阶段可以只有：

```text
namespace = "default"
```

- 第一阶段不要求立刻完成完整多租户权限系统，但 schema 和 identity 不得再假设“全局只有一个 namespace”。

## 5.2 Namespace 的未来职责

Namespace 将作为以下能力的基础：

- identity isolation
- ownership domain
- event filtering
- query filtering
- authorization
- quota
- resource visibility
- operation lifecycle isolation
- multi-project / multi-tenant control

## 5.3 Cross-namespace 行为

跨 Namespace 的引用、授权、Lease 或资源访问必须是**显式行为**。

默认原则：

> **Same namespace by default; cross namespace only by explicit policy.**

不得因为对象 `id` 相同而产生碰撞，也不得因为内部实现共享同一机器而绕过 Namespace 约束。

---

# 6. Capability：扩展体系核心

Capability 不得使用不断增长的中央 enum。

禁止：

```text
enum Capability {
    CUDA,
    ROCm,
    BF16,
    Docker,
    ...
}
```

应使用开放的 identifier + version + schema/properties：

```text
Capability {
    id
    version
    schema
    properties
}
```

示例：

```text
accelerator.compute
accelerator.memory
numeric.bf16
numeric.fp8
network.rdma

runtime.python
runtime.container
runtime.native

vendor.nvidia.cuda
vendor.nvidia.nvlink
vendor.amd.rocm

linux.cgroup
linux.namespace
```

Kernel 不解释 capability 的业务含义。

Kernel 只理解：

```text
Provider provides X
Operation/Worker requires X
X.version satisfies constraint
X.properties satisfies constraint
```

通用 capability 与 vendor capability 可以并存。

---

# 7. Resource：抽象资源，不等于 GPU

逻辑模型：

```text
Resource {
    ref
    class
    provider
    capabilities
    capacity
    topology
    attributes
}
```

Kernel 只理解 Resource 的抽象属性与可匹配约束。

例如：

```text
Resource #81
class = accelerator
capacity.memory = 80GB

capabilities:
    accelerator.compute
    numeric.fp16
    numeric.bf16
    vendor.nvidia.cuda
```

另一种 NPU 仍然只是 Resource。

新增硬件的黄金标准：

> **新增 Provider + Capability + Resource 描述即可，Kernel 核心无需理解厂商。**

---

# 8. Lease + Fence：资源所有权宪法

逻辑模型：

```text
Lease {
    ref
    resource
    holder
    generation
    fence_token
    scope
    expires_at
}
```

所有真正可能影响受控资源状态的操作，例如：

```text
start
attach
map
open
execute
publish protected endpoint
```

必须能够验证：

```text
Lease / fence token
```

当 Worker crash、Lease revoke 或 ownership generation 改变后：

```text
old actor
    ↓
old fence token
    ↓
invalid
```

旧 Worker 即使继续运行，也不能继续拥有资源控制权。

Lease/Fence 的语义属于冻结边界，不得被某个 Provider 自定义替代。

---

# 9. Provider：扩展世界的统一入口

Provider 的语义职责固定为三类：

## 9.1 Identity

```text
REGISTER_PROVIDER
```

回答：

> 我是谁？

## 9.2 Observation / Inventory

```text
PUBLISH_INVENTORY
UPDATE_INVENTORY
```

回答：

> 我当前实际拥有 / 观察到什么资源和能力？

## 9.3 Reconciliation

```text
RECONCILE_PROVIDER
```

回答：

> 当前 Provider reality 与 Kernel desired / recorded state 是否一致？如何收敛？

---

# 10. PublishInventory 与 ReconcileProvider：语义必须分离

这是 v1 的明确决策。

## 10.1 PublishInventory

`PublishInventory` 是**事实发布 / observation**。

它描述：

```text
Provider 当前看到了什么
Provider 当前拥有什么 Resource
Resource 当前有哪些 Capability / Capacity / Topology
```

## 10.2 ReconcileProvider

`ReconcileProvider` 是**状态收敛 / convergence**。

它处理：

```text
desired state
      vs
actual provider reality
```

并产生修复动作或状态更新。

## 10.3 强制约束

- 两者 **MUST** 保持独立 Semantic Action。
- 单节点实现中，两者 **MAY** 共用同一模块、线程、loop 或内部数据源。
- 不得因为当前单节点方便，而把两者永久合并成一个语义含糊的“provider update”。
- 未来跨机器资源池中，Inventory 可以由 Node/Agent 发布，而 Reconciliation 可以由 Controller/Scheduler 驱动；Contract 不需要因此改变。

原则：

> **语义拆开，实现暂时可以合并。**

---

# 11. Worker：执行与故障域

Worker 是 Kernel 认识的执行单元。

Worker 必须具有稳定逻辑身份和 lifecycle，不得等价于 OS PID。

Worker 可能：

```text
Registered
Ready
Running
Lost
Stopped
```

具体状态集合应保持小而稳定。

OS process / container / thread / remote executor 只是 Worker 的实现方式。

Kernel restart 后，即使 OS process 仍存在，也必须通过 recovery / reconciliation 重新证明它是否仍然代表当前 Worker generation。

---

# 12. Operation：保持极度抽象

Kernel 不认识：

```text
TrainOperation
InferenceOperation
DockerBuildOperation
ModelDownloadOperation
```

Kernel 只认识：

```text
Operation {
    ref
    owner
    executor
    kind
    state
    deadline
    parent
    metadata
}
```

`kind` 可以是开放 identifier：

```text
ai.train.sft
ai.inference.serve
runtime.container.build
dataset.convert
```

Kernel 不解释 kind。

## 12.1 Operation 状态机

核心状态保持小而稳定：

```text
Created
   ↓
Pending
   ↓
Running
   ↓
Completed / Failed / Cancelled / Lost
```

例如：

```text
LoadingWeights
CompilingGraph
WarmingUp
Downloading
```

属于 extension event 或 operation metadata，不进入 Kernel 核心状态机。

---

# 13. Command / Query / Event 三种语义必须分离

控制面必须明确区分：

```text
Command -> 请求改变世界
Query   -> 读取当前世界
Event   -> 描述已经发生的事实
```

## 13.1 Command 不等待长生命周期完成

例如：

```text
StartProvider(X)
```

应快速返回：

```text
Accepted
operation_id = ...
```

后续状态：

```text
ProviderStarting
ProviderReady
ProviderFailed
```

通过 Event Stream 观察。

不得把长生命周期状态机隐藏在一个长期阻塞 RPC 中。

---

# 14. Event Contract：固定为 Server-Streaming

`SubscribeEvents` 从 v1 起正式定义为：

> **Server-streaming event subscription**

而不是 polling API。

逻辑接口：

```text
SubscribeEvents(request) -> stream<Event>
```

Kotlin ergonomic projection 可以自然表示为：

```text
Flow<Event>
```

但 `Flow` 本身不是标准；标准是 server-streaming 语义。

## 14.1 Polling 的地位

Polling：

- **MUST NOT** 成为 canonical event semantics；
- **MAY** 作为旧客户端、测试、compat adapter 的外部包装；
- 不得迫使 Kernel 的事件模型退化成“客户端定时来问”。

---

# 15. Event Schema：顺序、Namespace 与重放

逻辑 Event：

```text
Event {
    sequence
    namespace
    subject
    kind
    timestamp
    payload
}
```

## 15.1 Sequence

Event 必须具有可排序、可恢复的 logical position。

`sequence` 表示事件日志位置。

Wire protocol 可以使用 opaque `cursor` 作为 resume token，但其语义必须对应确定的 event position。

## 15.2 Delivery semantics

默认采用：

> **ordered + replayable + at-least-once**

不承诺 exactly-once。

客户端必须允许：

- reconnect
- replay
- duplicate event
- deduplicate by sequence / cursor when necessary

不得通过“永不重复”换取无法可靠恢复。

## 15.3 Namespace filtering

订阅必须能够按 Namespace 过滤。

未来可以支持：

```text
SubscribeEvents(
    namespace = "team-a",
    after = cursor
)
```

或受权限控制的多 Namespace scope。

---

# 16. Snapshot + Stream：控制面一致性基线

Event streaming 必须与 Snapshot 形成可恢复组合。

推荐语义：

```text
GetSnapshot()
    ↓
state + cursor @ C100

SubscribeEvents(after = C100)
    ↓
C101
C102
C103
...
```

必须保证：

> Snapshot 已经包含其返回 cursor 之前的状态效果；客户端从该 cursor 之后订阅，可以重建一致的当前状态。

这使上层可以稳定构建：

```text
Snapshot
+
Incremental Events
=
Current State
```

Kotlin 可以进一步包装为：

```text
StateFlow<RuntimeState>
```

但 StateFlow 属于 SDK ergonomics，不进入 Semantic Contract。

---

# 17. Event Replay / Cursor 失效

事件日志允许有保留窗口，不要求无限保存。

如果客户端提供的 cursor 已过期或无法恢复：

Kernel 必须返回**明确的恢复信号**，例如语义上的：

```text
CursorExpired
ResnapshotRequired
```

客户端随后：

```text
GetSnapshot()
SubscribeEvents(after = new_cursor)
```

不得静默跳过丢失事件后继续伪装成连续流。

---

# 18. Event Backpressure

慢客户端不得阻塞 Kernel 核心状态机。

实现必须具有：

- bounded buffering
- cancellation
- slow-consumer detection
- reconnect/replay path

如果消费者严重落后，可以终止该 stream，并要求它从最近 cursor 重连或重新 snapshot。

原则：

> **A slow observer must never become kernel-wide backpressure.**

---

# 19. Reconciliation：Kernel 不只是 RPC Router

Kernel 的角色定义为：

> **Authority + State Reconciler**

系统必须假设以下情况一定会发生：

```text
Kernel crash
Provider crash
Worker crash
daemon restart
machine reboot
network interruption
```

因此不能依赖：

> “大家一直在线。”

Provider 可以报告：

```text
actual state
```

Kernel 保有：

```text
desired / recorded state
```

两者必须能够：

```text
compare
  ↓
reconcile
  ↓
converge
```

---

# 20. Daemon Restart / Orphan Recovery

跨 daemon 重启的孤儿进程处理正式进入 v1 开发基线。

注意：

> **OS Process 不是新的 Kernel 核心对象。**

Process identity 仅属于内部 runtime / recovery implementation。

## 20.1 Recovery 分三阶段

```text
Discover
   ↓
Classify
   ↓
Recover
```

### Discover

daemon 启动时发现：

- persisted worker/runtime record
- 当前仍存在的 managed processes
- Provider reality
- Kernel ledger 中仍存在的 ownership

### Classify

判断：

- 这个 process 是否确实由本系统创建；
- 它属于哪个 Worker / Provider / Namespace；
- 它属于哪个 generation；
- 它是否仍与当前 desired state 一致；
- 它是 stale、valid、unknown 还是 foreign。

### Recover

允许的恢复策略：

```text
Adopt
Terminate
Replace / Restart
Mark Lost
```

---

# 21. Runtime Process Identity：PID 永远不够

内部 recovery record 至少应能够表达：

```text
RuntimeProcessRecord {
    namespace
    worker_id
    worker_generation
    provider_id?
    pid
    process_start_identity
    launch_nonce
}
```

字段名和持久化格式不冻结，但语义要求如下：

- PID 不能单独作为进程身份；
- 必须能防止 PID reuse 误杀；
- 必须能绑定 logical Worker generation；
- 必须能判断旧 incarnation 是否 stale。

## 21.1 v1 初始恢复策略

第一阶段可以保守实现：

- 明确属于旧 generation 的 managed process：**terminate**
- ledger 认为存在但现实不存在的 Worker：**mark lost + reconcile**
- 无法证明属于本系统的 foreign process：**禁止 kill**
- 当前 generation 且身份验证完整的进程：可以先选择 terminate/restart；后续再增加 adopt

即：

> **先保证安全清理，再逐步增加无损 adopt。**

---

# 22. Crash Recovery 与 Lease/Fence 联动

典型恢复：

```text
Kernel ledger:
GPU0 -> Worker17

Provider reality:
Worker17 absent
```

则：

```text
mark WorkerLost
revoke Lease
increment generation / fence
emit WorkerLost
emit LeaseRevoked / ResourceRecovered
```

旧 Worker 即使重新出现，也不能凭旧 token 恢复权威。

---

# 23. Endpoint：Control Plane / Data Plane 分离

Kernel 负责：

```text
publish_endpoint
authorize_endpoint
revoke_endpoint
```

Kernel 完成：

- identity verification
- namespace / authorization check
- lease verification
- endpoint authorization

随后退出数据路径。

真正数据可以直接走：

```text
UDS
Shared Memory
CUDA IPC
RDMA
TCP
vendor-specific transport
```

原则：

> **Control through Kernel. Data around Kernel.**

Kernel 不传 tensor，不成为吞吐瓶颈。

---

# 24. Stable JVM SPI 与 Kotlin SDK

JVM 分两层。

## 24.1 Stable JVM SPI

必须保守、Java-compatible、低魔法：

```text
interface
immutable/final DTO
explicit version
plain lifecycle
```

避免把以下特性变成插件 ABI：

```text
复杂 Kotlin inline
compiler magic
context receiver
高度定制 DSL
```

## 24.2 Kotlin SDK

Kotlin SDK 可以自由提供：

```text
coroutine
Flow<Event>
StateFlow
DSL
extension functions
sealed results
builders
```

结构：

```text
Kotlin ergonomic API
        ↓
Stable JVM SPI
        ↓
Semantic Contract
```

SDK 可以演进，SPI 必须保守。

---

# 25. Protocol Negotiation 与 Compatibility

所有 projection 都必须能够表达版本协商。

必须区分：

- Semantic Contract version
- transport/protocol version
- payload/schema version
- SDK version

不得把它们混成一个版本号。

兼容性原则：

- additive capability 不要求增加 Kernel core version；
- 新 Provider 不要求增加 Kernel core version；
- SDK ergonomics 变化不要求增加 Kernel core version；
- breaking semantic change 才需要 Contract major version。

---

# 26. 什么应该冻结

当前可以作为 v1 开发冻结边界的内容：

```text
9 个核心对象
Identity model
Namespace first-class semantics
Generation semantics
Fence semantics
Worker lifecycle
Operation lifecycle
Resource ownership model
Lease semantics
Capability addressing model
Provider / Inventory / Reconcile semantic separation
Endpoint authorization model
Server-streaming Event semantics
Event ordering / replay semantics
Snapshot + Stream recovery model
Protocol negotiation
Reconciliation semantics
Crash / stale ownership recovery principles
```

这些内容一旦修改，需要按 Contract change 处理。

---

# 27. 什么不冻结

以下内容必须保留实现自由：

```text
具体 UDS message layout
具体 serialization
具体 Rust struct
具体 Kotlin class layout
具体 C struct layout（除稳定 ABI 已发布部分）
具体文件名
具体 module 名
具体 event payload
具体 persistence engine
具体 event log implementation
具体 transport
CUDA implementation
Python implementation
container implementation
Provider 内部 process model
```

不要因为“当前代码这样写了”就把实现偶然性升级成语义标准。

---

# 28. Source Organization Contract：源码组织规范

从 v1 起，源码组织也进入开发基线，但它属于**工程规范**，不是 Semantic Contract major-version 边界。

目标：

> **按语义职责拆分，而不是按行数机械拆碎。**

## 28.1 Daemon 不得继续成为 God File

daemon 顶层文件只负责：

```text
bootstrap
dependency wiring
lifecycle
session ownership
top-level dispatch
shutdown
```

它不应长期承载：

```text
resource ledger implementation
lease state machine
provider reconciliation
worker recovery
event log
endpoint authorization
UDS codec details
all command handlers
all persistence logic
```

## 28.2 文件规模规则

生产源码采用以下基线：

- **200–800 行**：推荐区间；
- **800–1200 行**：可以接受，前提是职责高度内聚；
- **>1200 行**：必须进行模块边界审查；
- **>1800 行**：新增功能前必须拆分，除非有书面理由；
- **>3000 行**：生产手写源码原则上禁止；
- generated code / vendor code / large test fixtures 可例外。

当前约 5000 行的 daemon 单文件：

> **必须在下一阶段功能开发前拆分。**

但拆分目标不是“每个文件都很小”，而是消除职责混杂。

---

# 29. 推荐模块边界

具体名字可以调整，但职责应接近：

```text
src/
├─ contract/
│  ├─ identity
│  ├─ types
│  └─ errors
│
├─ kernel/
│  ├─ authority
│  ├─ resources
│  ├─ leases
│  ├─ providers
│  ├─ workers
│  ├─ operations
│  ├─ endpoints
│  ├─ events
│  └─ reconcile
│
├─ daemon/
│  ├─ bootstrap
│  ├─ session
│  ├─ dispatch
│  └─ lifecycle
│
├─ transport/
│  └─ uds
│
├─ persistence/
│
└─ providers/
```

不要求严格使用这些目录名。

真正的约束是：

```text
Transport -> Projection -> Semantic services
```

而不是：

```text
Semantic core -> UDS-specific types
```

## 29.1 依赖方向

Kernel semantic modules **MUST NOT** 依赖：

- UDS message struct
- Kotlin DTO
- C ABI struct
- serialization-specific types

transport/projection 层负责 decode / encode 和类型转换。

---

# 30. 拆 daemon 的执行原则

第一次拆分必须以：

> **Mechanical refactor first, semantic change second.**

为原则。

## Phase A：只拆文件，不改语义

目标：

- 移动代码；
- 建立 module ownership；
- 缩小 daemon entry；
- 保持行为和测试结果不变。

禁止在这一阶段顺手重写：

- event semantics
- lease semantics
- reconciliation algorithm
- protocol shape

否则很难判断 regression 来自“拆文件”还是“改设计”。

## Phase B：再进行 Contract hardening

完成结构拆分后，再引入：

- Namespace fields
- unified ObjectRef / generation
- server-streaming Event
- snapshot + replay
- Inventory / Reconcile semantic split
- recovery state machine

---

# 31. 一个模块只拥有一种主要责任

一个源码模块应当主要回答一个问题，例如：

```text
leases      -> 谁现在拥有资源？
events      -> 已发生什么事实？
reconcile   -> recorded state 与 reality 如何收敛？
workers     -> 执行单元的逻辑生命周期是什么？
transport   -> 语义怎样在线上编码？
daemon      -> 这些模块怎样被启动和关闭？
```

如果一个文件同时需要回答 3–4 个问题，它通常已经需要拆分。

---

# 32. Change Control：以后所有开发围绕 Contract

任何新增功能先问：

```text
它能否由现有：
Worker
Operation
Resource
Lease
Capability
Endpoint
Event
Provider
组合表达？
```

如果可以：

> 不新增 Kernel 概念。

如果不可以：

必须提交最小 Contract Change 说明，至少回答：

1. 为什么现有对象无法组合表达？
2. 是否只是某个 Provider 的实现问题？
3. 是否把 data-plane detail 泄漏进 Kernel？
4. 是否影响 Namespace / Identity / Generation？
5. 是否影响 Event replay / reconciliation？
6. 是否会迫使所有 projection 同时增加新语义？
7. 是否存在兼容迁移路径？
8. 新增后能否继续通过“未来 NPU 零 Kernel 修改”测试？

---

# 33. Golden Tests：判断抽象是否仍然健康

以下测试应逐步进入 CI / integration test。

## 33.1 New Hardware Test

接入一个全新 NPU：

```text
Kernel core       0 semantic changes
C ABI             0 semantic changes
Stable JVM SPI    0 semantic changes
```

只增加：

```text
Provider
Capability
Resource description
optional SDK/plugin code
```

## 33.2 Stale Fence Test

```text
Worker A gets Lease
Worker A lost
Lease revoked
generation/fence changes
old Worker resumes
```

旧 token 必须被拒绝。

## 33.3 Event Resume Test

```text
snapshot @ C100
receive C101..C150
disconnect
reconnect after C150
```

客户端最终状态必须与 Kernel 当前状态一致。

## 33.4 Cursor Expiry Test

cursor 超出保留窗口时：

- 明确返回 resnapshot requirement；
- 不得静默漏事件。

## 33.5 Slow Consumer Test

一个 Event subscriber 停止消费时：

- Kernel 核心状态转换不得被阻塞；
- 允许 subscriber 被断开并恢复。

## 33.6 Namespace Collision Test

```text
namespace A / id X
namespace B / id X
```

必须是两个不同对象。

## 33.7 Namespace Isolation Test

未经显式授权：

```text
A cannot control B
```

## 33.8 Restart Recovery Test

daemon crash 后：

- 不误杀 foreign process；
- stale managed process 可识别；
- missing Worker 可标记 Lost；
- Lease/Fence 可恢复到安全状态；
- Event 能描述恢复结果。

## 33.9 Transport Replacement Test

替换 UDS serialization 或实现后：

- Semantic Contract 不变；
- Kernel domain logic 不需要理解新 wire type。

---

# 34. 当前开发顺序

从现在起建议按以下顺序推进。

## P0 — 先做结构整理

1. 给当前基线打 tag / 固定测试结果。
2. 拆分约 5000 行 daemon。
3. daemon entry 只保留 wiring / lifecycle / top-level dispatch。
4. 不改变外部行为。

## P1 — 固化身份与事件边界

1. Namespace 进入 ObjectRef / request context。
2. generation 语义统一。
3. `SubscribeEvents` 改为 canonical server-streaming。
4. Event sequence / cursor / replay 明确。
5. 加 `GetSnapshot + cursor`。

## P1 — 固化 Provider 边界

1. `PublishInventory` 与 `ReconcileProvider` 保持独立 semantic action。
2. 单节点可共享实现。
3. 不提前实现完整分布式调度器。

## P1/P2 — Recovery

1. 持久化最小 runtime ownership metadata。
2. startup discover / classify / recover。
3. 第一版优先安全 terminate stale managed process。
4. 后续再增加 validated adopt。

## P2 — JVM / Kotlin 上层

在上述语义稳定后再继续扩大：

```text
Stable JVM SPI
Kotlin Flow<Event>
StateFlow state projection
higher-level DSL
```

---

# 35. 当前明确决策表

| 议题 | v1 决策 |
|---|---|
| 核心词汇 | 保持约 9 个对象 |
| SubscribeEvents | **Server-streaming** |
| Polling | 仅可做兼容 adapter，不是 canonical semantics |
| Event replay | **必须支持 sequence/cursor 恢复语义** |
| Snapshot + Stream | **进入标准恢复模型** |
| PublishInventory / ReconcileProvider | **语义拆分** |
| 单节点两者实现 | 可以共用内部实现 |
| Namespace | **一等字段，现在进入模型** |
| 初始 Namespace | 可仅使用 `default` |
| PID | 仅内部实现细节 |
| orphan recovery | **进入开发基线** |
| orphan 第一版策略 | stale managed process 优先安全 terminate |
| process adopt | 后续能力，不要求首版完成 |
| daemon 5000 行单文件 | **下一功能阶段前拆分** |
| 文件拆分依据 | 语义职责优先，行数作为 guardrail |
| JVM | Stable SPI 与 Kotlin ergonomic SDK 分层 |
| 新硬件扩展 | Provider + Capability，原则上不改 Kernel |

---

# 36. 最终判断标准

第一版 Kernel 是否真正接近完成，不以“功能数量”判断。

只问：

> **未来的新硬件、AI runtime、训练框架、容器系统、远程执行后端，能否通过组合已有的 Provider + Worker + Operation + Resource + Lease + Capability + Endpoint + Event 表达，而无需继续扩大 Kernel vocabulary？**

以及：

> **Kernel / daemon / Provider / Worker 任意重启后，系统能否通过 Identity + Namespace + Generation + Lease/Fence + Event Replay + Reconciliation 重新收敛，而不是依赖进程永远不死？**

如果这两个答案都基本是“能”，则 Kernel 已进入可以长期维护和向上构建的形态。

---

# 37. 一句话总纲

```text
Semantic Contract first.
Authority stays in Kernel.
Execution may fail.
State must reconcile.
Events must replay.
Data bypasses Kernel.
Extensions enter through Provider + Capability.
Namespaces isolate ownership.
Generation + Fence invalidate the past.
Implementation may change; semantics must remain stable.
```
