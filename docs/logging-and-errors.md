<!--
================================================================================
SYNCHRONIZED DOCUMENT - DO NOT EDIT DIRECTLY IN THIS REPOSITORY
Canonical Source: Cyrene-Workspace/docs/standards/logging-and-errors.md
Synchronized By: scripts/sync-logging-spec.py
================================================================================
-->

# Cyrene 日志、错误码与诊断规范

- **文档类型**：跨仓工程规范草案，不是实施完成报告
- **规范版本**：0.1
- **状态**：`REVIEW_READY`；经仓库 owner 接受后作为后续实现约束
- **实现状态**：本文件不代表日志系统已实现、已接入或已验收
- **建议归属**：Cyrene-Workspace 的现有 standards / governance 文档区域
- **适用范围**：Platform、Plugins、各 Product、Studio / Navigator 与安装运行工具
- **首轮实现重点**：Platform Rust 守护进程及 RC 用户流程涉及的诊断边界

---

## 0. 目标、术语与实施边界

本规范要解决的不是“让每个文件都有日志”，而是：

1. 关键操作的开始、结果、失败原因、恢复动作和最终资源状态能够解释。
2. 相同错误具有稳定机器标识；用户得到可操作提示，开发者能定位对应记录。
3. 日志不泄露敏感信息，不污染 stdout 协议，不阻塞关键监管与资源清理。
4. Product / Plugin 可以独立演进业务错误，不因增加错误码而要求发布新版 Platform。
5. 真实执行、模拟执行、未执行和未知结果不得相互冒充。

本文“必须 / 不得”为接受本规范后的强制要求；“建议”为允许有理由偏离的默认方案。

> [!IMPORTANT]
> 输入审计中的 crate 数量、文件数量、`let _ =` 数量和代码行号属于调查快照，不是永久规范。落库者不得把这些数字写成当前源码已经复核的事实。
> 
> 本次落库任务仅创建或对齐规范及最少的导航引用。不实施日志埋点，不批量修改错误处理，不修改状态机、鉴权、协议、依赖锁、CI 或安装程序。

---

## 1. 架构与所有权

### 1.1 统一格式，分散维护语义

共同规范只定义：
- 日志字段、事件名和错误码的命名规则；
- 上下文关联、序列化、级别、脱敏与输出约束；
- 诊断查询、支持材料导出与验收要求。

各 owner 定义和维护自身语义：

| Owner | 负责的内容 |
| :--- | :--- |
| **Platform** | 通用资源、Node、Worker、Package、Lease / Fence、准入、监管与清理诊断 |
| **Plugins / 能力契约 owner** | 能力调用错误、具体实现诊断、第三方供应商错误映射 |
| **Product** | 数据、训练、部署、路由、评估、会话等自身业务错误与业务操作结果 |
| **Studio / Navigator UI** | 展示错误、关联诊断和提供操作入口；不重新判定底层生命周期结果 |
| **日志采集 / 查询组件** | 接收、存储、过滤、检索与访问控制；不拥有业务状态 |

- 不得在 Platform 建立包含所有 Product / Plugin 业务错误的大枚举或全局业务 SDK。
- 新增 evaluator、Provider、数据类型或模型参数，正常情况下只改变其 owner 的错误定义，不要求改 Platform。
- 共同规范归属不等于运行依赖：Product 不得为了记录日志而依赖 Workspace 源码 checkout；也不得仅为日志初始化重新引入 Platform 业务 SDK。

### 1.2 技术实现保持薄层

Rust 可以复用很薄的初始化、格式化、过滤和脱敏辅助模块。该模块不得：
- 定义 Product 业务对象；
- 转发业务 payload；
- 接管重试、任务调度、租约、认证或恢复；
- 要求所有语言使用同一个私有日志 RPC。

Python、.NET、JVM 可保留各自成熟日志设施，在输出边界映射到共同字段。是否抽出独立共享包，要由真实复用需求决定，不为落规范先建新仓库或新框架。

---

## 2. 不得混淆的数据种类

| 数据种类 | 用途 | 不得替代的对象 |
| :--- | :--- | :--- |
| **运行日志** | 排障、操作过程解释、运行诊断 | 不替代 Product / Kernel 状态 |
| **Trace / Span** | 关联一次分布式操作及其子操作 | 不替代认证身份或业务任务 ID |
| **指标** | 速率、延迟、显存、资源利用、错误计数 | 不把每次采样都输出成 INFO |
| **Product 事件** | UI 订阅、进度、状态变化、loss / step / checkpoint 等事实 | 不从易变的日志文案反向推导 |
| **安全审计记录** | 谁对什么资源做了何种安全敏感操作、结果如何 | 不与可随意采样的 DEBUG 日志等同 |
| **恢复日志 / 状态 journal / WAL** | 崩溃恢复、事务或生命周期权威 | 不用普通 tracing 日志替代 |

必须保持：
1. Lease 释放成立，取决于已有权威状态与物理清理确认，不取决于是否打印了 released。
2. UI 不得通过匹配日志字符串判断训练成功、部署 READY 或资源已释放。
3. 第三方 trainer 只能提供文本时，由对应 adapter 解析为类型化 Product 事件，UI 不直接解析其 stderr。
4. 普通日志清理、轮转、支持包导出，不得修改 journal、数据库、checkpoint 或 Artifact。
5. 安全审计可共用基础技术，但其保留、访问、投递与失败策略必须独立声明；不得套用普通诊断采样策略而静默丢弃。
6. 如某操作要求“持久审计后才能执行”，该要求属于安全 / 业务策略；日志 Agent 不得自行新增或移除这种门禁。

---

## 3. 错误码与事件命名

### 3.1 新错误码的推荐形式

新错误码使用：
```text
<OWNER>.<DOMAIN>.<REASON>
```

示例：
- `PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED`
- `PLATFORM.WORKER.CLEANUP_UNCONFIRMED`
- `YIELD.RUNTIME.DEPENDENCY_UNAVAILABLE`
- `REACTOR.DEPLOYMENT.READINESS_PROBE_FAILED`
- `EXCHANGE.AUTH.API_KEY_REVOKED`

> [!NOTE]
> 这些只是命名示例，不表示相关代码已经存在，也不授权替换现有公开错误码。

强制规则：
- 使用稳定、可搜索、有限集合的语义标识。
- 不给每行日志分配错误码；正常事件无须错误码。
- 不在错误码中加入请求 ID、模型名、GPU 型号、文件路径、版本号或源码行号。
- 不用错误码编码日志级别、HTTP 状态或“是否可自动重试”。
- 已有公开 API / capability contract 的稳定错误码优先保留；兼容映射必须显式，不允许全仓静默改名。
- 移动文件、改语言、换存储实现，不应改变错误码含义。
- 已发布错误码不得复用为不同含义；废弃需记录替代关系与支持范围。
- 本轮不建立全局数字号段，不平行维护字符串与数字两套权威编号。

### 3.2 事件名

事件名推荐小写点分：
- `platform.worker.started`
- `platform.lease.release_deferred`
- `platform.node.reconnected`
- `yield.training.checkpoint_saved`
- `reactor.deployment.ready`

- 事件名表示事件类型，而非实例身份；不得动态拼接资源 ID。
- 事件结构的字段意义需要稳定。可兼容增加可选字段；不得在同一事件名下悄悄改变关键字段语义。

### 3.3 错误目录

每个 owner 选择一个权威来源：现有类型化错误定义，或现有 owner-scoped 契约目录。不同时手工维护多份错误清单。

错误定义至少说明：
1. `code`；
2. 触发条件与明确不适用的情况；
3. `owner`；
4. 默认安全说明；
5. 是否允许暴露给调用者；
6. 允许记录的上下文字段；
7. 现有 API / RPC 映射（如适用）；
8. 兼容、废弃和替代规则。

严重程度和恢复建议可给默认指导，但不得机械决定每次调用的级别或重试行为。

全局索引和面向开发者的错误文档应由 owner 定义生成；新增业务错误不要求修改 Platform 中央表。

---

## 4. 结构化记录模型

这是 Cyrene 的逻辑记录模型，不要求每种语言都实现新日志框架，也不声称普通 JSON 文件就是 OTLP。

生产服务的第一方日志应可输出为一行一个完整 JSON 对象的 UTF-8 NDJSON。统一出口只采用一种字段布局，不同时人工维护 camelCase / snake_case 两套等价记录。

### 4.1 字段

| 字段 | 要求 |
| :--- | :--- |
| `schema_version` | 第一方结构化记录必填；表示本日志格式版本，不是产品版本 |
| `timestamp` | 必填；事件时间，UTC / RFC 3339 |
| `level` | 必填；`TRACE` / `DEBUG` / `INFO` / `WARN` / `ERROR` |
| `event.name` | 第一方关键事件必填；稳定事件类型 |
| `service.name` | 必填；逻辑服务名，不是用户主机名 |
| `service.instance.id` | 服务进程必填；每次启动唯一，避免 PID 重用造成混淆 |
| `message` | 安全的人类可读摘要，不作为程序分支依据 |
| `trace_id` / `span_id` | 有追踪上下文时记录；无上下文则省略，不伪造 |
| `attributes` | 受控的结构化上下文集合 |

`attributes` 按实际场景包含：
- `error.code`、经过分类的 `cause.kind`；
- `operation_id`、`request_id`；
- 当前 owner 合法持有的资源 ID；
- `attempt`、`duration_ms`、`timeout_ms`；
- `previous_state`、`observed_state`、`desired_state`；
- `outcome`、`recovery.action`；
- 经许可的 `generation`、非秘密的 `fence` 身份引用；
- `execution.mode`（只有相关场景才记录 `real` / `simulated` / `test`）。

未知字段不得用 `0`、空串或 `false` 假装已观测到。未发生的结果不得提前写成成功。需要明确表达不确定性时，使用 `outcome: unknown` 等约定。

`duration_ms` 使用单调时钟计算；跨进程事件顺序不得仅由墙上时钟排序推断。

构建版本可由构建流程注入服务资源属性，或启动事件记录一次。不得人工往每行日志和普通架构文档重复写 SHA。

### 4.2 OpenTelemetry 映射

须保留到 OpenTelemetry 逻辑字段的清晰映射：
- `timestamp` → `Timestamp`；
- `level` → `SeverityText` / `SeverityNumber`；
- `message` → `Body`；
- `event.name` → `EventName`；
- `service` 身份 → `Resource`；
- `trace` / `span` → `TraceId` / `SpanId`；
- 其他上下文 → `Attributes`。

采用现有 exporter 时由适配层完成转换，不创建新的私有遥测协议。首个 RC 不要求部署 Collector 或托管日志后端。

### 4.3 记录大小

默认建议单条结构化记录上限为 32 KiB、message 上限为 4 KiB、原因链最多 8 层。以上为项目默认建议，不是外部标准的要求；实施者须与现有运行预算核对后固定。

超限时必须保持 JSON 合法，保留 event、error code、追踪和资源标识等核心字段，标记截断；不得把序列化字节直接截断成破损 JSON。

第三方进程输出还应有单来源速率与总量预算，避免日志输出成为资源耗尽入口。

---

## 5. 示例：清理失败但不能伪报资源释放

以下完全为合成示例：

```json
{
  "schema_version": 1,
  "timestamp": "2026-09-21T12:00:00Z",
  "level": "ERROR",
  "event.name": "platform.lease.release_deferred",
  "service.name": "cyrene-kernel",
  "service.instance.id": "synthetic-instance-1",
  "message": "Could not persist release intent; allocation remains reserved.",
  "attributes": {
    "error.code": "PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED",
    "operation_id": "synthetic-operation-1",
    "worker_id": "synthetic-worker-1",
    "lease_id": "synthetic-lease-1",
    "attempt": 2,
    "cause.kind": "io.permission_denied",
    "outcome": "deferred",
    "recovery.action": "watchdog_reconcile",
    "allocation_released": false
  }
}
```

其中 `allocation_released: false` 只有在权威状态证明尚未释放时才能记录；状态未知时不得编造。

之后若恢复成功，新增恢复结果事件并关联同一 operation / lease。不得覆盖、删除或改写首次失败记录来制造“从未失败”的历史。

---

## 6. 级别、频率与重复记录

| 级别 | 用途 |
| :--- | :--- |
| **TRACE** | 极细调试，默认关闭，不得依赖它才能知道关键结果 |
| **DEBUG** | 有界诊断、详细尝试过程 |
| **INFO** | 关键生命周期、用户可见操作结果、正常取消、恢复成功 |
| **WARN** | 可恢复退化、连接丢失、重试中或需要关注但尚未终局失败 |
| **ERROR** | 操作失败、关键持久化失败、清理无法确认、服务无法继续履责 |

规则：
1. 错误码和日志级别独立。同一错误原因在一次中间尝试与最终操作失败中可有不同级别。
2. 预期客户端取消、已关闭接收方，不应自动作为 ERROR。
3. 取消后遗留 Worker / Lease 等清理失败，必须与正常取消单独记录。
4. 同一异常不要沿调用栈每层重复输出完整 ERROR；选择负责处理或形成最终结果的边界记录，其他层补上下文或关联。
5. 重连 / watchdog 不得每次轮询刷 ERROR。记录首次异常、有限频率的摘要和恢复结果；摘要带重试次数、持续时间或被压制次数。
6. 状态机只在有意义的决策、状态转移或失败时记录，不能每轮 reconcile 重复打印相同 INFO。
7. 高基数的 operation、resource、trace ID 不能直接成为指标标签。
8. 普通诊断可受预算限制，但关键事件不得被随意的随机采样策略排除；队列溢出仍可能丢失，必须另行计数，不宣称零丢失。

---

## 7. 跨层错误、API、重试与 UI

### 7.1 API 错误契约

已有 Product / RPC 错误格式优先保留。新增或统一 HTTP 错误格式时，可采用 RFC 9457 Problem Details；错误码与诊断标识作为安全的扩展字段。

必须区分：
- 稳定机器错误码；
- 面向用户的安全说明；
- 仅对授权运维可见的诊断详情。

客户端不得通过解析 message / detail 文本判断错误类型。

`error.code` 与 API 的 `code` 语义须一致或有显式映射，不得分别建立含义不同的日志码与接口码。

跨服务可以映射为更适合调用者的错误，但必须保留安全的下游原因和关联信息；不得把所有错误压成无法诊断的 `EXECUTION_FAILED`。

未知下游错误必须安全回退，UI 不崩溃，也不原样展示敏感异常正文。

### 7.2 流式请求

HTTP 头或流已经开始发送后，错误必须通过既有流式协议的错误 / 终止语义表达；不得把普通 JSON 错误对象或日志行塞进 SSE、gRPC、JSON-RPC 或二进制数据流。

取消请求、实际停止、资源回收、最终状态是不同事实，不得合并成一个“已取消所以已释放”。

### 7.3 重试策略

日志系统和错误码目录不得执行自动重试。

恢复决策由操作 owner 按幂等性、当前状态和下游事实决定，例如：
- `safely_retry`；
- `query_state_first`；
- `fix_configuration`；
- `user_action_required`；
- `none` / `unknown`。

不得仅凭 HTTP 5xx、TIMEOUT 或连接中断认定操作没有执行，更不得自动重复训练启动、发布或其他副作用操作。

### 7.4 UI 最低要求

UI 的操作失败展示至少具备：
- 安全、可读的说明；
- 存在时展示稳定错误码；
- 可复制的 operation / diagnostic 关联标识；
- 来自 Product 的合法恢复入口。

不要求 UI 展示堆栈或完整 stderr。不能因为网络超时直接显示“训练失败并已释放资源”，应回读 Product 状态或显示结果未知。

---

## 8. 上下文关联与信任边界

- HTTP 的分布式追踪采用 W3C Trace Context；其他传输映射到已有协议的合法 metadata / 管理上下文。
- `request_id` 表示一次请求，`operation_id` 表示长操作，`training_run_id` / `deployment_id` 等表示业务资源；不要混为一个 ID。
- 重试应保留稳定操作身份，并增加 `attempt`；子请求可有独立 request / span。
- 崩溃后恢复可产生新 trace，但必须通过既有资源 / operation 身份关联；不得伪造不存在的旧 span。
- 同一资源的不同并发操作也必须可区分。
- 外部 trace / baggage / correlation headers 都是不可信输入：校验格式、限制长度，不能用来认证、授权、提升日志级别或跨租户查询。
- 进程传递只使用已有允许的通用上下文渠道；不得为追踪泄露父进程全部环境，或无计划地修改所有业务 payload。
- 查询日志的权限不能仅凭“知道 trace_id”；必须核对调用者对相应租户 / workspace / resource 的访问权。
- 插件提供的 owner、service、actor 等字段不能直接成为可信审计身份，收集层应保留实际进程/连接来源与插件自报信息的区别。
- 不允许全局可变变量作为当前 request 上下文；异步任务必须正确传播上下文，避免串到其他请求。

---

## 9. Rust 实现约束

建议 Rust 采用 `tracing` + `tracing-subscriber`，具体兼容版本由仓库工具链和锁文件确定。

### 9.1 初始化

- 库 crate 只产生事件 / span，不安装全局 subscriber，不修改进程全局 panic hook。
- 每个 binary 在启动入口尽早初始化一次，包括启动失败路径。
- 嵌入式场景由 host 控制 subscriber；不能偷偷覆盖既有 subscriber。
- 初始化失败需可诊断，不得无声退回“没有日志”。
- log 兼容桥只在实际依赖需要时使用；禁止循环桥接和重复记录。
- 默认服务模式为 JSON / INFO，开发模式可选人类可读文本。
- 日志级别配置必须可验证；无效配置不能静默变成全部关闭或全部 TRACE。
- `RUST_LOG` 若保留，只控制明确允许的过滤行为，不改变数据脱敏策略。

### 9.2 埋点

- `#[instrument]` 默认按 `skip_all` 加字段白名单的思路使用，禁止自动 Debug 整个请求 / 配置。
- 不在资源状态锁持有期间做慢格式化、磁盘 / 网络日志 I/O。
- 不在 token chunk、每轮 heartbeat、每次显存采样上默认输出 INFO。
- 不把日志函数包装成另一套异常、重试或业务事件框架。
- 不强制为每个 helper 函数创建 span；聚焦有诊断价值的操作边界。

---

## 10. stdout / stderr 与子进程协议隔离

强制要求：
1. **第一方 runtime / daemon / CLI 的诊断日志固定到 stderr**，所有级别一致。
2. **stdout 保留现有机器可读响应、命令正常结果和协议流**。
3. 不使用未指定 writer 的默认初始化来假定输出方向；官方 `tracing-subscriber::fmt` 默认输出 stdout，实施时必须显式指定 stderr。
4. Cargo build.rs 的指令输出、hash CLI 的结果、fixture 的协议标记，不按普通日志机械替换。
5. stdio MCP / plugin 的 stdout 是协议时，监管层不得把日志写进去，或把协议内容作为普通日志泄露。
6. 必要的第三方 stdout / stderr 捕获使用独立 reader 并有界消费；不得因不读取其中一条 pipe 而阻塞子进程。
7. 第三方输出标记来源并限长、限速、脱敏；不能把其自报“PASS / READY / RELEASED”直接作为权威状态。
8. 第三方非结构化行可封装成外部输出事件，但不得声称是原生类型化事件。
9. 日志换行、控制字符和 ANSI 转义必须安全处理，避免伪造额外日志行或终端控制效果。

---

## 11. 首批关键路径覆盖

优先覆盖这些行为，不以“所有源码文件都有日志”为完成标准：

| 路径 | 必须能解释的事实 |
| :--- | :--- |
| **服务启动 / 停止** | 实例身份、配置 profile、安全的构建版本、就绪或失败原因、关闭结果 |
| **Node 连接 / 重连** | 首次失联、重试摘要、退避、恢复结果；不得把怀疑网络分区写成已证实 |
| **Worker 启动** | 授权来源引用、启动阶段、实际 PID/实例、成功或失败结果 |
| **Lease / 持久化** | 意图、提交/落盘结果、资源是否保留、失败后恢复动作 |
| **终止分类 / reconcile** | 观测依据、分类、选择动作及是否改变权威状态 |
| **sandbox 清理** | 请求终止、宽限期、必要的强制终止、进程树与资源清理确认 |
| **Package / 插件运行** | 安装、准备器、启动、健康、升级/回滚失败；不打印完整环境 |
| **控制请求与 relay** | 拒绝原因、预期断开与意外发送失败的区别、清理结果 |
| **Product 用户操作** | 相同 operation 下的开始、参数校验、交接、结果与恢复提示 |
| **网关安全操作** | key 创建/撤销、发布/撤销路由、权限拒绝，按安全审计策略记录 |

纯函数状态机优先由提交其决策的调用边界记录，不为覆盖率强迫纯函数加入 I/O。

---

## 12. let _ =、异常与 panic

### 12.1 被忽略结果

每个关键路径中忽略的结果须归类，而不是全部加 `error!`：
- **EXPECTED_BENIGN**：如对已关闭接收方的通知失败；可无日志或限频 DEBUG，保留理由。
- **BEST_EFFORT**：不影响业务安全的附加操作；必要时 WARN / 计数。
- **RECOVERABLE_FAILURE**：记录失败与已安排的恢复，最终恢复另记。
- **SAFETY_CRITICAL**：落盘、清理确认、授权或状态一致性失败；必须维持原 fail-closed 语义并提供诊断。
- **BEHAVIOR_BUG**：原本就错误地忽略了结果；交给对应功能 owner 修行为，不能仅加日志假装修好。

### 12.2 Panic / 终止

- 普通 Rust panic 在 unwind / abort 模式下均先调用 panic hook；因此“abort 必然无输出”不是正确假设。
- hook 由 binary/host 控制，采用最小、经过脱敏的紧急诊断路径，不能依赖异步队列在进程退出时正常刷新。
- hook 不得再次获取可能已损坏的资源状态锁，不得执行长网络操作，也不得把任意 panic payload 原文输出到公开日志。
- 不承诺捕获 SIGKILL、断电、进程直接 abort 等所有终止方式；由外部监管记录观测到的退出事实，原因未知时标 unknown。
- 不为“避免没有日志”把 `.expect()` 全改成吞错或使用不可信的中毒状态。
- 不为日志改动切换项目 panic 策略，或擅自引入 `catch_unwind` 改变已有安全语义。
- 非正常退出前的最后一条日志只提供线索，不等于完整业务清理证明。

---

## 13. 隐私、密钥与不可信内容

默认禁止记录：
- Token、PAT、密码、私钥、cookie、签名 URL、数据库连接串；
- 完整请求/响应、prompt、模型回答、训练样本、embedding 向量；
- 完整环境变量、命令行参数数组、配置对象或任意 Debug dump；
- 可直接授予权限的 fence / lease / session 凭证。

默认采用字段白名单。允许按已认证授权身份记录内部资源引用，但不得把用户输入的身份字段当真。

路径、主机名、IP、模型私有名称等可能属于受限运维元数据：默认优先记录逻辑引用或脱敏值；确需原值时，由受限诊断 profile 定义用途、权限和保留期，不能自动进入公共报告。

严格禁止以“开启 DEBUG”为理由关闭密钥保护。即使显式内容诊断开启，也不能记录凭据。

支持包 / CI artifacts / Git 文档默认视为可能外发：
- 导出需先脱敏、限制范围并可预览；
- 不默认外传；
- 不自动上传到公共 issue；
- 不使用可猜测短密钥的普通 hash 冒充安全脱敏；
- 脱敏后的材料不能冒充字节级原始收据。

第三方文本和请求参数必须 JSON 安全编码，防止换行和分隔符注入。日志查看器不得把内容当 HTML、命令或可自动执行的建议。

---

## 14. 输出、持久化、轮转与故障预算

### 14.1 本地优先、后端可替换

首个 RC 必须能在无云日志账号、无 Collector 的情况下保留可排障记录。

推荐：
- 应用输出 stderr；
- 受管安装由服务管理器或现有收集层保存；
- 不由服务管理器管理的运行方式，提供一个受控文件 sink；
- 后续可增加 OTel / 托管后端，不成为本地运行前置条件。

应用或收集器二选一负责同一文件的轮转；不得双方同时处理。

禁止每个 crate 单独开日志文件。日志位置来自部署 profile，不从源码 checkout 或个人 home 路径推导。

### 14.2 有界预算

实施方案必须明确给出：
- 单条大小、每来源速率；
- 队列记录数和/或字节预算；
- flush / shutdown 的最长等待时间；
- 文件大小、保留文件数/时间、每主机总磁盘预算；
- 满队列、满磁盘、无权限、输出断开时的行为；
- 丢弃/截断/导出失败如何计数和提示。

缺少有限预算的实现不得标为 RC 完成。不能用无限内存队列或无限重试弥补外部日志服务故障。

### 14.3 故障行为

- 普通诊断 sink 故障不应阻塞 Worker 清理、Lease 回收和已有恢复过程。
- 有界异步队列是优先考虑方案，但要明确会丢什么、怎样计数；不得将非阻塞等同于零丢失。
- logger 内部失败不得递归调用同一故障 logger 形成错误风暴。
- 正常关闭应在有限预算内刷新；超时明确报告 best-effort 结果，不无限等待。
- 日志不可用时，应有可见的观测退化状态或紧急诊断，不伪装为“完整诊断能力正常”。
- 需要持久审计的特殊操作遵循已有明确策略；普通日志组件不得临时决定拒绝或允许业务。
- 清理日志只能作用于明确受管的日志目录和文件，禁止广泛通配删除状态库、checkpoint、Artifact 或任意 worktree。
- `tracing-appender` 的非阻塞队列默认可丢日志；禁用丢失会引入背压。实施者必须显式选择并测试，不能依赖默认值猜测。

---

## 15. 配置、查询与部署 profile

必须分清：
- `development`：可读文本、可选 DEBUG，仍保护密钥；
- `managed/installed`：结构化输出、有限持久化和访问权限；
- `test/CI`：独立记录接收器、可重复断言，无外部日志账号；
- `restricted diagnostics`：显式授权、有限范围/期限，非默认配置。

配置优先级应只有一套，遵循现有应用规则；如无现有规则，规定为：
```text
显式命令参数 > 受支持环境配置 > 配置文件 > 默认值
```
并测试实际解析结果。

不因为日志设计重写各 Product 配置系统，不自动修改用户系统全局日志设置。

日志读取/下载须鉴权、限制租户和资源范围。开发模式仅 loopback 并不自动构成所有场景的授权策略。

RC 不要求新建日志 Web 产品；可以先通过系统日志工具、受限文件和已有诊断入口查询。若接入 UI，需按 operation/resource 查询，不把整个宿主日志公开给普通用户。

---

## 16. 与功能闭环 Agent 的分工

| 事项 | 功能 Agent | 日志 Agent |
| :--- | :--- | :--- |
| **安装 / doctor** | 检查真正安装的目标运行环境、所选 profile 和依赖 | 记录各检查结果与失败上下文 |
| **训练 / 部署 / 网关** | 修请求参数保存、交接、就绪、取消、恢复等真实行为 | 记录关键步骤、关联操作与诊断 |
| **错误定义** | 确定领域含义与合法恢复方式 | 按统一字段输出，不重新解释 |
| **UI** | 展示稳定错误码、安全说明、合法操作和诊断关联 | 提供可查询的记录 |
| **生命周期缺陷** | 修资源泄漏或状态不一致 | 揭示缺陷；不以增加日志代替修复 |
| **证据** | 证明操作行为正确 | 证明失败可解释、敏感信息不泄露、输出不污染 |

特别要求：
1. doctor 检查所选 profile 的真实依赖，不以系统 Python import 结果代替实际 trainer/serving 环境。
2. “只安装网关”的 profile 不因缺训练器而失败；训练 profile 不得在缺训练器时报告全部正常。
3. 功能日志的错误码和 UI/API 映射需要两位 owner 对齐，不能各写一套。
4. 同仓同文件只有一个 writer；跨 Agent 修改须先交接，不因日志遍布全仓而覆盖对方工作。

---

## 17. RC 实现验收标准

以下是后续实现任务的验收要求，不是本次文档落库必须执行的代码测试。

| ID | 最低验收内容 |
| :--- | :--- |
| **LOG-01** | 关键 binary 启动成功和配置失败都有结构化记录；库不抢装全局 subscriber |
| **LOG-02** | JSON 字段、时间和大小限制有正负测试；正常事件不强制带错误码 |
| **LOG-03** | stdout 的 JSON / stdio / fixture 协议保持可解析，无日志插入 |
| **LOG-04** | 可控注入落盘/清理失败后，记录准确原因、保留状态和恢复动作；不虚报 RELEASED |
| **LOG-05** | 重连/退避有限频率输出；最终恢复可关联，日志量受控 |
| **LOG-06** | 正常取消不产生误导性 ERROR；清理失败可单独定位 |
| **LOG-07** | 并发请求和 async task 关联不串线；长操作可通过稳定 ID 查到多次尝试 |
| **LOG-08** | 合成 Token、URL 凭据、环境密钥、prompt 和训练样本不会进入默认日志或导出包 |
| **LOG-09** | 日志注入、超长文本和第三方输出风暴不会污染格式或耗尽无界资源 |
| **LOG-10** | 满磁盘、无写权限、慢接收器、队列满和 Collector 离线不使资源清理无限等待；丢失可计数 |
| **LOG-11** | 普通 panic 的紧急诊断经过脱敏；异常终止不声称日志完整或业务已完成清理 |
| **LOG-12** | 日志轮转与支持包清理不影响状态库、Artifact 和 checkpoint |
| **LOG-13** | API/UI 的稳定错误码与操作关联能找到对应诊断；未知错误安全处理 |
| **LOG-14** | 文档列明已覆盖的服务、事件和 OS/profile；未测项不写 PASS |

验收要求：
- 不通过精确匹配完整自然语言文案来证明错误语义；断言结构化字段和行为。
- 不用 grep 到 `tracing::` 的次数或“文件覆盖率”代替行为测试。
- 预期不同级别、采样与限制均需在测试配置中可控。
- CI 的故障注入使用隔离临时目录、测试进程和合成数据，不破坏开发者真实环境。
- 没改 GPU 算法/训练行为时，优先用轻量 Worker/fixture 验证日志；只有触及真实执行路径的修改才安排对应硬件验收。
- 模拟故障可以证明特定日志行为，但不能冒充真实 NVIDIA / 多机 / Windows 验收。
- 证据记录 SOURCE/IMPLEMENTATION、LOCAL_TEST、HOSTED_CI、OS_PROFILE 等实际范围；文档通过不等于日志系统通过。

---

## 18. 非目标与禁止扩张

首轮不做：
1. 新的大一统遥测 RPC / 业务事件总线；
2. 自研 Elasticsearch/Loki 替代品或完整日志 SaaS；
3. 为日志新增独立账号、租户、计费和认证体系；
4. 要求所有业务流量重新经过 Platform；
5. 全仓业务异常类型重写；
6. 全部语言同一实现、全部第三方输出都强制结构化；
7. 每条 INFO 分配错误码；
8. 每个函数埋点或每个 token 打印一行；
9. 在日志任务中实现训练器、部署向导或新 GPU 适配器；
10. 通过日志文本重建权威状态；
11. 把架构文档改成不断追逐当前 SHA 的手工证据账本。

---

## 19. 文档落库任务要求

把本规范交给落库 Agent 时，执行范围如下：
1. 先读取当前 Workspace 拓扑、规范目录与文档治理规则；不假定历史路径仍正确。
2. 查找是否已有 Logging / Error / Observability 规范，选定唯一权威文档位置并对齐，避免创建冲突版本。
3. 只修改规范正文、必要的索引链接和最小 ADR 引用；不修改生产代码、CI、Cargo/uv/npm 锁文件。
4. 对现有错误码、API 格式或追踪机制，仅做必要的只读冲突检查；不扩成全仓重审计。
5. 未能确定的事实写成待实现核验；不得把候选字段写成已经上线。
6. 正常保护脏文件与并行工作；不用 reset/clean/force push 清场。
7. 如仓库流程允许文档提交，使用独立文档分支与正常提交；不擅自合并共享分支或开启功能实施。
8. 执行文档链接、现有格式与必要示例检查；不为纯文档任务构建全部 Rust/GPU 运行环境。
9. 最终只交付：规范路径、所改文件、已有规范冲突与处理、文档检查结果、实际 Git 交付状态。

完成后停止，等待日志实现任务与功能任务分工。

建议落库路径（按现有约定调整，不是新增强制目录）：
- `Cyrene-Workspace/docs/standards/logging-and-errors.md`

Platform 与 Product 文档只引用共同规范；各域错误码实现和目录仍由对应 owner 维护。

---

## 20. 标准参考与项目决策的区别

本规范中的所有权、命名示例、首轮范围、大小预算建议、目录位置和验收 ID 都是 Cyrene 的拟定工程决策，不声称由外部标准强制要求。

外部参考只用于下列技术依据：
- **[S1] OpenTelemetry Logs Data Model**：区分 EventName、Severity、Body、Resource、Attributes、Trace/Span 字段。
- **[S2] tracing-subscriber fmt**：结构化事件的格式输出；默认 writer 为 stdout，需显式改为 stderr。
- **[S3] tracing instrument**：默认记录参数，因此需要按参数敏感性排除并显式选字段。
- **[S4] tracing-appender NonBlockingBuilder**：有界队列的 lossy / backpressure 选择。
- **[S5] Rust panic::set_hook**：普通 panic 的 hook 在 abort/unwind 前执行；默认输出 stderr。
- **[S6] W3C Trace Context**：跨服务 trace 上下文格式。
- **[S7] RFC 9457**：HTTP Problem Details，不把自然语言 detail 当机器错误契约。
- **[S8] OWASP Logging Cheat Sheet**：敏感信息保护、不可信事件输入、日志访问控制与故障测试。

参考地址：
- [S1] <https://opentelemetry.io/docs/specs/otel/logs/data-model/>
- [S2] <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/>
- [S3] <https://docs.rs/tracing/latest/tracing/attr.instrument.html>
- [S4] <https://docs.rs/tracing-appender/latest/tracing_appender/non_blocking/struct.NonBlockingBuilder.html>
- [S5] <https://doc.rust-lang.org/stable/std/panic/fn.set_hook.html>
- [S6] <https://www.w3.org/TR/trace-context/>
- [S7] <https://www.rfc-editor.org/rfc/rfc9457.html>
- [S8] <https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html>

具体库版本由实现仓当前工具链、依赖锁和 CI 确定。本规范不要求为了追随 latest 文档而擅自升级依赖。
---
<!-- Chinese Translation / 中文翻译 -->

# Cyrene 日志、错误码与诊断规范

- **文档类型：** 跨仓工程规范草案，不是实施完成报告。
- **规范版本：** 0.1。
- **状态：** `REVIEW_READY`；经仓库 owner 接受后作为后续实现约束。
- **实现状态：** 本文件不表示日志系统已经实现、接入或验收。
- **建议归属：** Cyrene-Workspace 现有 standards/governance 文档区域。
- **适用范围：** Platform、Plugins、各 Product、Studio/Navigator 和安装运行工具。
- **首轮重点：** Platform Rust 守护进程以及 RC 用户流程涉及的诊断边界。

## 0. 目标、术语与实施边界

本规范的目标不是“让每个文件都有日志”，而是：

1. 能解释关键操作何时开始、结果如何、为何失败、如何恢复以及最终资源状态。
2. 相同错误有稳定的机器标识；用户获得可操作提示，开发者可定位对应记录。
3. 日志不泄露敏感信息、不污染 stdout 协议，也不阻塞关键监管和资源清理。
4. Product/Plugin 可独立演进业务错误，不必为了增加错误码而发布新版 Platform。
5. 真实执行、模拟执行、未执行和未知结果不会相互冒充。

“必须/不得”在接受本规范后为强制要求；“建议”是允许在有理由时偏离的默认方案。

> **重要：** 输入审计中的 crate 数量、文件数、`let _ =` 数量和代码行号都是调查快照，不是永久规范。落库时不得把这些数字描述成已对当前源码复核的事实。本次落库任务只创建或对齐规范并补最少导航引用；不实施日志埋点、不批量改错误处理，也不修改状态机、鉴权、协议、依赖锁、CI 或安装程序。

## 1. 架构与所有权

### 1.1 统一格式，分散维护语义

共同规范只定义日志字段、事件名和错误码命名规则，上下文关联、序列化、级别、脱敏和输出约束，以及诊断查询、支持材料导出和验收要求。各 owner 分别定义并维护自身语义：

| Owner | 负责内容 |
| --- | --- |
| **Platform** | 通用资源、Node、Worker、Package、Lease/Fence、准入、监管和清理诊断 |
| **Plugins / 能力契约 owner** | 能力调用错误、具体实现诊断及第三方供应商错误映射 |
| **Product** | 自身的数据、训练、部署、路由、评估、会话等业务错误和操作结果 |
| **Studio / Navigator UI** | 展示错误、关联诊断并提供操作入口；不重新判定底层生命周期结果 |
| **日志采集/查询组件** | 接收、存储、过滤、检索和访问控制；不拥有业务状态 |

- 不得在 Platform 建立覆盖所有 Product/Plugin 业务错误的大枚举或全局业务 SDK。
- 新增 evaluator、Provider、数据类型或模型参数时，通常只更新对应 owner 的错误定义，不要求修改 Platform。
- 共同规范的归属不等于运行依赖。Product 不得为记录日志而依赖 Workspace 源码 checkout，也不得仅为日志初始化重新引入 Platform 业务 SDK。

### 1.2 技术实现保持轻薄

Rust 可复用精简的初始化、格式化、过滤和脱敏辅助模块；该模块不得定义 Product 业务对象、转发业务 payload、接管重试/任务调度/租约/认证/恢复，也不得要求所有语言使用同一个私有日志 RPC。

Python、.NET、JVM 可保留各自成熟的日志设施，只需在输出边界映射到共同字段。是否抽出独立共享包，由真实复用需求决定；不能为了发布规范先建仓库或新框架。

## 2. 不得混淆的数据种类

| 数据种类 | 用途 | 不能替代 |
| --- | --- | --- |
| **运行日志** | 排障、解释操作过程和运行诊断 | Product/Kernel 状态 |
| **Trace / Span** | 关联分布式操作及其子操作 | 认证身份或业务任务 ID |
| **指标** | 速率、延迟、显存、资源利用率和错误数 | 不应把每次采样都写成 INFO |
| **Product 事件** | UI 订阅、进度、状态变化、loss/step/checkpoint 等事实 | 不得从易变日志文案反向推导 |
| **安全审计记录** | 记录操作者、资源、安全敏感操作及结果 | 可随意采样的 DEBUG 日志 |
| **恢复日志 / 状态 journal / WAL** | 崩溃恢复、事务或生命周期权威 | 普通 tracing 日志 |

必须遵守以下规则：

1. Lease 是否释放，由既有权威状态和物理清理确认决定，不取决于是否打印了 `released`。
2. UI 不得匹配日志字符串来判断训练成功、部署 READY 或资源已释放。
3. 第三方 trainer 只提供文本时，由对应 adapter 解析成类型化 Product 事件；UI 不直接解析其 stderr。
4. 清理或轮转普通日志、导出支持包，不得修改 journal、数据库、checkpoint 或 Artifact。
5. 安全审计可共用基础技术，但其保留、访问、投递和失败策略须独立声明；不能套用普通诊断采样而静默丢弃。
6. 若某操作规定“先持久化审计才能执行”，这属于安全/业务策略；日志 Agent 不得自行添加或移除该门禁。

## 3. 错误码与事件命名

### 3.1 推荐的新错误码格式

新错误码采用 `<OWNER>.<DOMAIN>.<REASON>` 格式，例如：

```text
PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED
PLATFORM.WORKER.CLEANUP_UNCONFIRMED
YIELD.RUNTIME.DEPENDENCY_UNAVAILABLE
REACTOR.DEPLOYMENT.READINESS_PROBE_FAILED
EXCHANGE.AUTH.API_KEY_REVOKED
```

这些只是命名示例，不代表相应代码已存在，也不授权替换已有公开错误码。

强制规则：错误码必须稳定、可搜索且语义集合有限；不为每条日志分配错误码，正常事件不需要错误码；错误码不能包含 request ID、模型名、GPU 型号、文件路径、版本号或源码行号；不能用它编码日志级别、HTTP 状态或“是否自动重试”。已有公开 API/capability contract 的稳定错误码优先保留；兼容映射必须显式，禁止全仓静默改名。移动文件、更换语言或存储实现不能改变错误码含义。已发布错误码不得复用为其他含义；弃用时记录替代项和支持范围。本轮不建立全局数字号段，也不并行维护字符串与数字两套权威编号。

### 3.2 事件名

事件名建议使用小写点分形式，例如 `platform.worker.started`、`platform.lease.release_deferred`、`platform.node.reconnected`、`yield.training.checkpoint_saved`、`reactor.deployment.ready`。

- 事件名表示事件类型，不是实例身份；不得动态拼接资源 ID。
- 事件结构的字段含义须稳定。可兼容地增加可选字段；不得在同一事件名下悄然改变关键字段语义。

### 3.3 错误目录

每个 owner 选择一种权威来源：已有类型化错误定义，或已有 owner-scoped 契约目录；不得人工维护多份平行错误清单。

错误定义至少说明：`code`；触发条件和明确不适用情形；`owner`；默认安全说明；能否向调用者展示；允许记录的上下文字段；已有 API/RPC 映射（如有）；兼容、弃用和替代规则。

可为严重程度和恢复建议提供默认指导，但不能机械决定每次调用的级别或重试行为。全局索引及开发者错误文档应由 owner 定义生成；新增业务错误不要求修改 Platform 中央表。

## 4. 结构化记录模型

以下是 Cyrene 的逻辑记录模型，不要求每种语言采用新日志框架，也不表示普通 JSON 文件等于 OTLP。

生产服务的第一方日志应可输出为 UTF-8 NDJSON，每行是一个完整 JSON 对象。统一出口只用一种字段布局，不同时人工维护 camelCase 与 snake_case 两套等价记录。

### 4.1 字段

| 字段 | 要求 |
| --- | --- |
| `schema_version` | 第一方结构化记录必填；表示日志格式版本，不是产品版本 |
| `timestamp` | 必填；UTC / RFC 3339 事件时间 |
| `level` | 必填；`TRACE` / `DEBUG` / `INFO` / `WARN` / `ERROR` |
| `event.name` | 第一方关键事件必填；稳定的事件类型 |
| `service.name` | 必填；逻辑服务名，不是用户主机名 |
| `service.instance.id` | 服务进程必填；每次启动唯一，避免 PID 重用混淆 |
| `message` | 安全的人类可读摘要，不得用作程序分支依据 |
| `trace_id` / `span_id` | 有追踪上下文时记录；没有则省略，不得伪造 |
| `attributes` | 受控的结构化上下文集合 |

`attributes` 可按实际场景包含 `error.code`、分类后的 `cause.kind`、`operation_id`、`request_id`、当前 owner 合法持有的资源 ID、`attempt`、`duration_ms`、`timeout_ms`、`previous_state`/`observed_state`/`desired_state`、`outcome`、`recovery.action`、获准使用的 `generation` 和非秘密 fence 身份引用，以及只在相关场景使用的 `execution.mode`（`real`/`simulated`/`test`）。

未知字段不得用 `0`、空字符串或 `false` 伪装成已观测事实；未发生的结果不能预先写成成功。需要表达不确定性时使用 `outcome: unknown` 等约定。`duration_ms` 使用单调时钟计算；不能只依赖墙上时钟推断跨进程事件顺序。构建版本可由构建流程注入服务资源属性，或在启动事件中记录一次；不得人工把 SHA 重复写入每行日志和普通架构文档。

### 4.2 OpenTelemetry 映射

清晰映射到 OpenTelemetry 逻辑字段：`timestamp` 对应 `Timestamp`；`level` 对应 `SeverityText`/`SeverityNumber`；`message` 对应 `Body`；`event.name` 对应 `EventName`；service identity 对应 `Resource`；trace/span 对应 `TraceId`/`SpanId`；其余上下文对应 `Attributes`。使用现有 exporter 时由 adapter 转换，不创建私有遥测协议。首个 RC 不要求部署 Collector 或托管日志后端。

### 4.3 记录大小

建议单条结构化记录不超过 32 KiB、message 不超过 4 KiB、原因链不超过 8 层。它们是项目默认建议，不是外部标准要求；实现者要与现有运行预算核对后固定。

超限时必须保持 JSON 合法，保留 event、error code、trace 和资源标识等核心字段并标明截断；不得直接截断序列化字节而生成破损 JSON。第三方进程输出还须限制单来源速率和总量，防止日志输出成为资源耗尽入口。

## 5. 示例：清理失败但不能误报资源释放

以下是纯合成示例：

```json
{
  "schema_version": 1,
  "timestamp": "2026-09-21T12:00:00Z",
  "level": "ERROR",
  "event.name": "platform.lease.release_deferred",
  "service.name": "cyrene-kernel",
  "service.instance.id": "synthetic-instance-1",
  "message": "无法持久化释放意图；allocation 仍处于预留状态。",
  "attributes": {
    "error.code": "PLATFORM.LEASE.RELEASE_INTENT_PERSIST_FAILED",
    "operation_id": "synthetic-operation-1",
    "worker_id": "synthetic-worker-1",
    "lease_id": "synthetic-lease-1",
    "attempt": 2,
    "cause.kind": "io.permission_denied",
    "outcome": "deferred",
    "recovery.action": "watchdog_reconcile",
    "allocation_released": false
  }
}
```

只有权威状态证明 allocation 尚未释放，才能记录 `allocation_released: false`；状态未知时不能编造。之后恢复成功时，增加恢复结果事件并关联同一 operation/lease。不得覆盖、删除或改写首次失败记录，制造“从未失败”的历史。

## 6. 级别、频率与重复记录

| 级别 | 用途 |
| --- | --- |
| **TRACE** | 默认关闭的极细粒度调试；不得依靠它确认关键结果 |
| **DEBUG** | 有界诊断和详细尝试过程 |
| **INFO** | 关键生命周期、用户可见操作结果、正常取消及恢复成功 |
| **WARN** | 可恢复退化、连接丢失、重试中或需关注但尚未终局失败 |
| **ERROR** | 操作失败、关键持久化失败、清理无法确认或服务无法继续履责 |

错误码和日志级别相互独立：同一错误原因在中间尝试和最终失败时可记录为不同级别。预期客户端取消或接收方已关闭不应自动作为 ERROR。取消后残留 Worker/Lease 等清理失败，须与正常取消分开记录。同一异常不应沿调用栈每层重复输出完整 ERROR；由负责处理或形成最终结果的边界记录，其他层仅补上下文或关联。重连/watchdog 不得每轮刷 ERROR；记录首次异常、有限频率摘要和恢复结果，摘要提供重试数、持续时间或抑制数。状态机只在有意义决策、状态转换或失败时记录，不得每次 reconcile 循环重复输出同一 INFO。高基数 operation/resource/trace ID 不得成为指标标签。普通诊断可受预算限制，但关键事件不得被任意随机采样排除；队列溢出仍可能丢失，必须另行计数，不能宣称零丢失。

## 7. 跨层错误、API、重试与 UI

### 7.1 API 错误契约

优先保留已有 Product/RPC 错误格式。新增或统一 HTTP 错误格式时可采用 RFC 9457 Problem Details，并用安全扩展字段承载错误码和诊断标识。必须分清稳定机器错误码、面向用户的安全说明，以及仅授权运维可见的诊断详情。客户端不得解析 message/detail 文本来判断错误类型。日志 `error.code` 和 API `code` 的语义须相同或显式映射，不能建立含义不同的日志码和接口码。跨服务可映射成更适合调用者的错误，但须保留安全的下游原因和关联，不能把一切压成无法诊断的 `EXECUTION_FAILED`。未知下游错误须安全 fallback；UI 不崩溃，也不原样显示敏感异常正文。

### 7.2 流式请求

HTTP header 或流开始发送后，错误必须通过既有流协议的错误/终止语义表达；不得把普通 JSON 错误对象或日志行塞入 SSE、gRPC、JSON-RPC 或二进制数据流。请求取消、实际停止、资源回收和最终状态是不同事实，不能合并成“已取消所以已释放”。

### 7.3 重试策略

日志系统和错误码目录不得执行自动重试。恢复由操作 owner 根据幂等性、当前状态和下游事实决定，例如 `safely_retry`、`query_state_first`、`fix_configuration`、`user_action_required`、`none` 或 `unknown`。仅凭 HTTP 5xx、TIMEOUT 或连接中断，不能认定操作没有执行，也不能自动重复启动训练、发布或其他副作用操作。

### 7.4 UI 最低要求

UI 操作失败至少展示安全易懂的说明、存在时展示稳定错误码、可复制的 operation/diagnostic 关联标识，以及 Product 提供的合法恢复入口。不要求 UI 展示堆栈或完整 stderr。网络超时后不能直接说“训练失败且资源已释放”，应回读 Product 状态或显示结果未知。

## 8. 上下文关联与信任边界

- HTTP 分布式追踪使用 W3C Trace Context；其他传输映射到既有协议允许的 metadata/管理上下文。
- `request_id` 表示一次请求，`operation_id` 表示长操作，`training_run_id`/`deployment_id` 等表示业务资源；不能混成同一 ID。
- 重试沿用稳定操作身份并增加 `attempt`；子请求可有独立 request/span。
- 崩溃后恢复可创建新 trace，但须通过既有资源/operation identity 关联；不得伪造不存在的旧 span。
- 同一资源上的并发操作也须能区分。
- 外部 trace/baggage/correlation header 均是不可信输入：校验格式、限制长度，不得用于认证、授权、提升日志级别或跨租户查询。
- 进程间传递只使用已有允许的通用上下文渠道；不得为追踪泄露父进程完整环境或无计划改动所有业务 payload。
- 日志查询权限不能仅凭“知道 trace_id”获得；必须验证调用者有权访问相应 tenant/workspace/resource。
- 插件提供的 owner/service/actor 字段不能直接成为可信审计身份；收集层要区分实际进程/连接来源与插件自报值。
- 不得使用全局可变变量作为当前 request context；异步任务需正确传播上下文，避免串入其他请求。

## 9. Rust 实现约束

建议 Rust 使用 `tracing` 和 `tracing-subscriber`；兼容版本由仓库工具链与依赖锁确定。

### 9.1 初始化

库 crate 只产生 event/span，不安装全局 subscriber、不改进程全局 panic hook。每个 binary 在启动入口尽早初始化一次，包括启动失败路径。嵌入式场景由 host 控制 subscriber，不得暗中覆盖已有实例。初始化失败须能诊断，不可无声退回无日志状态。只有实际依赖需要时才启用 log 兼容桥，禁止循环桥接或重复记录。服务默认使用 JSON/INFO，开发环境可选人类可读文本。日志级别配置必须可验证；无效配置不能静默关闭所有日志或启用全量 TRACE。若保留 `RUST_LOG`，它只控制明确允许的过滤行为，不能改变脱敏策略。

### 9.2 埋点

- `#[instrument]` 默认采用 `skip_all` 加字段 allowlist；禁止自动 Debug 整个 request/config。
- 持有资源状态锁时，不做慢速格式化、磁盘或网络日志 I/O。
- 默认不为 token chunk、每轮 heartbeat 或每次显存采样输出 INFO。
- 不把日志函数包装成另一套异常、重试或业务事件框架。
- 不要求每个 helper function 都创建 span；重点覆盖有诊断价值的操作边界。

## 10. stdout/stderr 与子进程协议隔离

必须遵守：第一方 runtime/daemon/CLI 诊断一律写 stderr，所有级别一致；stdout 保留现有机器可读响应、命令正常结果和协议流。不能用未指定 writer 的默认初始化来猜输出方向；官方 `tracing-subscriber::fmt` 默认写 stdout，必须显式指定 stderr。Cargo build.rs 指令输出、hash CLI 结果和 fixture 协议标记不能机械替换为普通日志。stdio MCP/plugin 的 stdout 属于协议时，监管层不能向其中写日志，也不能把协议内容作为普通日志泄露。必要时以独立 reader 捕获第三方 stdout/stderr，并有界消费，不能因不读某一 pipe 而阻塞子进程。第三方输出应标记来源并限长、限速、脱敏；不能把自报的“PASS/READY/RELEASED”视为权威状态。第三方非结构化行可封装为外部输出事件，但不能宣称为原生类型化事件。换行、控制字符和 ANSI 转义须安全处理，避免伪造日志行或终端控制效果。

## 11. 首批关键路径覆盖

优先覆盖有意义的行为；“所有源码文件都有日志”不是完成标准。

| 路径 | 必须解释的事实 |
| --- | --- |
| 服务启动/停止 | 实例身份、配置 profile、安全的构建版本、就绪/失败原因和关闭结果 |
| Node 连接/重连 | 首次失联、重试摘要、退避、恢复；不得把疑似网络分区写成已确认事实 |
| Worker 启动 | 授权来源引用、启动阶段、实际 PID/实例及成功/失败结果 |
| Lease/持久化 | 意图、提交/落盘结果、资源是否保留和失败后的恢复动作 |
| 终止分类/reconcile | 观测依据、分类、所选动作及权威状态是否变化 |
| sandbox 清理 | 终止请求、宽限期、必要的强制终止、进程树和资源清理确认 |
| Package/插件运行 | 安装、准备器、启动、健康、升级/回滚失败；不得输出完整环境 |
| 控制请求与 relay | 拒绝原因、预期断开与意外发送失败的区别、清理结果 |
| Product 用户操作 | 同一 operation 下的开始、参数校验、交接、结果与恢复提示 |
| Gateway 安全操作 | key 创建/撤销、route 发布/撤销、权限拒绝，按安全审计策略记录 |

纯函数状态机优先由提交决策的调用边界记录；不能为了覆盖率强迫纯函数加入 I/O。

## 12. `let _ =`、异常与 panic

### 12.1 被忽略的结果

关键路径中被忽略的结果必须分类，不能一律补 `error!`：

- **EXPECTED_BENIGN：** 例如通知已关闭接收方失败；可不记录或限频为 DEBUG，并保留原因。
- **BEST_EFFORT：** 不影响业务安全的附加操作；必要时记录 WARN/计数。
- **RECOVERABLE_FAILURE：** 记录失败和已安排的恢复；恢复最终结果另行记录。
- **SAFETY_CRITICAL：** 落盘、清理确认、授权或状态一致性失败；必须维持原 fail-closed 语义并提供诊断。
- **BEHAVIOR_BUG：** 原本就不应忽略的结果；由功能 owner 修复行为，不能只加日志假装解决。

### 12.2 Panic/终止

- 普通 Rust panic 在 unwind/abort 模式下都会先调用 panic hook；“abort 一定不会输出”是错误假设。
- hook 由 binary/host 控制；使用最小、经过脱敏的紧急诊断路径，不能依赖进程退出时仍能正常刷新的异步队列。
- hook 不得再次取得可能已损坏的资源状态锁、执行耗时网络操作或公开输出任意 panic payload。
- 不承诺捕获 SIGKILL、断电、进程直接 abort 等所有终止；由外部监管记录观察到的退出事实，原因未知就标记 unknown。
- 不得为避免“无日志”而把 `.expect()` 全改成吞错，或使用不可信的中毒状态。
- 不因日志修改切换项目 panic 策略，也不得擅自用 `catch_unwind` 改变已有安全语义。
- 异常退出前的最后一条日志只提供线索，不是完整业务清理证明。

## 13. 隐私、密钥与不可信内容

默认禁止记录 Token、PAT、密码、私钥、cookie、签名 URL、数据库连接串；完整 request/response、prompt、模型回答、训练样本、embedding 向量；完整环境变量、命令行参数数组、配置对象或任意 Debug dump；以及可直接授予权限的 fence/lease/session credential。

默认使用字段 allowlist。允许按已认证授权身份记录内部资源引用，但不能信任用户输入的身份字段。

路径、主机名、IP 和模型私有名称可能属于受限运维 metadata；默认优先记逻辑引用或脱敏值。确需原值时，由受限诊断 profile 说明用途、权限和保留期，不能自动放入公开报告。

绝不能以“打开 DEBUG”为由关闭密钥保护；即使显式开启内容诊断，也不得记录 credential。

Support package/CI artifact/Git 文档默认视为可能对外提供：导出前需脱敏、限范围并可预览；不得默认外传或自动上传到公共 issue；普通 hash 不能冒充对可猜测短密钥的安全脱敏；脱敏材料不能冒充字节级原始收据。

第三方文本和请求参数须 JSON 安全编码，防止换行/分隔符注入。日志查看器不得把内容当 HTML、命令或可自动执行建议。

## 14. 输出、持久化、轮转与故障预算

### 14.1 本地优先，后端可替换

首个 RC 必须能在没有云日志账号和 Collector 时保留可排障记录。建议应用写 stderr，由受管安装的服务管理器或现有收集层保存；未由服务管理器管理的运行方式可提供一个受控文件 sink；后续可增加 OTel/托管后端，但不能成为本地运行前置条件。

同一文件的轮转由应用或收集器二选一负责，不能两边同时处理。禁止每个 crate 单独建日志文件。日志位置来自部署 profile，不从源码 checkout 或个人 home 路径推导。

### 14.2 有界预算

实施方案必须明确单条大小、每来源速率、队列记录/字节预算、flush/shutdown 最大等待时间、文件大小/保留数或时长/每主机总磁盘预算，以及队列满、磁盘满、权限不足、输出断开时的行为，并说明如何计数和提示丢弃/截断/导出失败。

没有有限预算的实现不能标为 RC 完成。不能用无限内存队列或无限重试来弥补外部日志服务故障。

### 14.3 故障行为

- 普通诊断 sink 故障不能阻塞 Worker 清理、Lease 回收和已有恢复流程。
- 有界异步队列可优先考虑，但必须说明会丢什么及如何计数；non-blocking 不等于零丢失。
- logger 内部失败不能递归调用同一个失效 logger 形成错误风暴。
- 正常关闭须在有限预算内 flush；超时要明确报告 best-effort 结果，不能无限等待。
- 日志不可用时要有可见的观测退化状态或紧急诊断，不能伪装成诊断完整。
- 需要持久审计的特殊操作遵循既有明确策略；普通日志组件不能临时决定拒绝或允许业务。
- 清理只能作用于明确受管的日志目录/文件，禁止广泛通配删除状态库、checkpoint、Artifact 或任意 worktree。
- `tracing-appender` 非阻塞队列默认可能丢日志；关闭丢失会引入背压。实施者必须明确选择并测试，不能猜测默认值。

## 15. 配置、查询与部署 profile

必须区分 `development`（可读文本、可选 DEBUG，但仍保护密钥）、`managed/installed`（结构化输出、有限持久化和访问权限）、`test/CI`（独立接收器和可重复断言，无外部日志账号），以及 `restricted diagnostics`（显式授权、范围/期限有限，默认不启用）。

配置优先级必须只有一套并遵循现有应用规则；若没有既有规则，则依次为：

```text
显式命令参数 > 受支持环境配置 > 配置文件 > 默认值
```

并测试实际解析结果。不因日志设计重写各 Product 配置系统，也不自动修改用户系统全局日志设置。

日志读取/下载必须鉴权，并限制 tenant/resource 范围。开发模式只绑定 loopback 不自动成为所有情形的授权策略。RC 不要求新建日志 Web 产品；可先使用系统日志工具、受限文件和既有诊断入口。若接入 UI，须按 operation/resource 查询，不能把整个主机日志公开给普通用户。

## 16. 与功能闭环 Agent 的分工

| 事项 | 功能 Agent | 日志 Agent |
| --- | --- | --- |
| **安装/doctor** | 检查目标实际运行环境、所选 profile 和依赖 | 记录检查结果和失败上下文 |
| **训练/部署/Gateway** | 修复请求参数保存、交接、就绪、取消、恢复等真实行为 | 记录关键步骤、操作关联和诊断 |
| **错误定义** | 确定领域含义和合法恢复方式 | 按统一字段输出，不重新解释 |
| **UI** | 展示稳定错误码、安全说明、合法操作和诊断关联 | 提供可查询记录 |
| **生命周期缺陷** | 修复资源泄漏或状态不一致 | 暴露缺陷；不能用加日志替代修复 |
| **证据** | 证明操作行为正确 | 证明失败可解释、无敏感信息泄露且输出不污染协议 |

特别要求：doctor 检查所选 profile 的真实依赖，不以系统 Python import 结果代替 trainer/serving 环境。只安装 Gateway 的 profile 不会因缺训练器而失败；训练 profile 缺训练器时不能报告全部正常。功能日志错误码及 UI/API 映射须由两个 owner 对齐。同仓同文件只有一个 writer；跨 Agent 修改先交接，不能因为日志遍布仓库就覆盖他人工作。

## 17. RC 实现验收标准

下表是后续实现任务的验收要求，不是此次文档落库必须执行的代码测试。

| ID | 最低验收内容 |
| --- | --- |
| **LOG-01** | 关键 binary 启动成功和配置失败都有结构化记录；库不抢装全局 subscriber |
| **LOG-02** | JSON 字段、时间和大小限制均有正/负测试；普通事件不强制错误码 |
| **LOG-03** | stdout JSON/stdio/fixture protocol 保持可解析且没有插入日志 |
| **LOG-04** | 注入落盘/清理失败后准确记录原因、保留状态和恢复动作，不误报 RELEASED |
| **LOG-05** | 重连/退避输出受限；恢复可关联，日志量受控 |
| **LOG-06** | 正常取消不产生误导 ERROR；清理失败可单独定位 |
| **LOG-07** | 并发请求和 async task 关联不串线；长操作可由稳定 ID 找到多次尝试 |
| **LOG-08** | 合成 Token、URL 凭据、环境密钥、prompt 和训练样本不会进入默认日志或导出包 |
| **LOG-09** | 日志注入、超长文本和第三方输出风暴不会污染格式或耗尽无界资源 |
| **LOG-10** | 满盘、无写权限、慢接收器、队列满和 Collector 离线不会让资源清理无限等待；丢失可计数 |
| **LOG-11** | 普通 panic 的紧急诊断已脱敏；异常终止不声称日志完整或业务清理完成 |
| **LOG-12** | 日志轮转与支持包清理不影响状态库、Artifact 和 checkpoint |
| **LOG-13** | API/UI 稳定错误码和 operation 关联能找到对应诊断；未知错误安全处理 |
| **LOG-14** | 文档列出覆盖服务、事件和 OS/profile；未测项目不写 PASS |

验收须断言结构化字段和行为，不能精确匹配完整自然语言来证明错误语义；不能以 grep 到 `tracing::` 的次数或“文件覆盖率”代替行为测试。测试配置要能控制预期级别、采样和限额。CI 故障注入使用隔离临时目录、测试进程和合成数据，不破坏开发环境。未修改 GPU 算法/训练行为时优先用轻量 Worker/fixture 验证；只有触及真实执行路径才安排对应硬件验收。模拟故障不能冒充 NVIDIA/多机/Windows 实测。证据分别记录 SOURCE/IMPLEMENTATION、LOCAL_TEST、HOSTED_CI、OS_PROFILE 范围；文档完成不代表日志系统通过。

## 18. 非目标与禁止扩张

首轮不做：新建一统遥测 RPC/业务事件总线；自研 Elasticsearch/Loki 替代品或日志 SaaS；为日志新增账号、租户、计费和认证体系；让所有业务流量重新经过 Platform；全仓重写业务异常类型；要求所有语言实现相同框架或所有第三方输出结构化；给每条 INFO 分配错误码；给每个函数埋点或每个 token 打一行；在日志任务中实现 trainer、部署向导或新 GPU adapter；用日志文本重建权威状态；把架构文档改成不断追 SHA 的手工证据账本。

## 19. 文档落库任务要求

将本规范交给落库 Agent 时：先读取当前 Workspace 拓扑、规范目录和文档治理规则，不假定旧路径仍有效；查找已有 Logging/Error/Observability 规范并选出唯一权威文档，避免冲突版本；只修改规范正文、必要索引链接和最小 ADR 引用，不改生产代码、CI 或 Cargo/uv/npm 锁文件；只读检查现有错误码/API/追踪机制的必要冲突，不扩成全仓重审；不确定事实写成待实现核验，不把候选字段说成已上线；保护脏文件和并行工作，不用 reset/clean/force push 清理现场；若流程允许文档提交，使用独立文档分支和正常提交，不擅自合并共享分支或开启功能实施；执行文档链接、现有格式和必要示例检查，不为纯文档工作构建全部 Rust/GPU 环境；最终只交付规范路径、修改文件、规范冲突及处理、文档检查结果和实际 Git 交付状态。

落库后应停止并等待日志实现任务与功能任务分工。建议落库位置为 `Cyrene-Workspace/docs/standards/logging-and-errors.md`，但应按当前约定调整，不是强制新增目录。Platform 和 Product 文档只引用共同规范；各域错误码实现和目录仍由对应 owner 维护。

## 20. 标准参考与项目决策的区别

本规范中的所有权、命名示例、首轮范围、大小预算建议、目录位置和验收 ID 均是 Cyrene 拟定的工程决策，不声称由外部标准强制规定。外部参考只提供以下技术依据：

- **[S1] OpenTelemetry Logs Data Model：** 区分 EventName、Severity、Body、Resource、Attributes、Trace/Span 字段。
- **[S2] tracing-subscriber fmt：** 结构化事件格式输出；默认 writer 为 stdout，必须显式改为 stderr。
- **[S3] tracing instrument：** 默认记录参数，因此须按敏感性排除并显式选择字段。
- **[S4] tracing-appender NonBlockingBuilder：** 有界队列的 lossy/backpressure 选择。
- **[S5] Rust panic::set_hook：** 普通 panic 的 hook 在 abort/unwind 前执行；默认输出 stderr。
- **[S6] W3C Trace Context：** 跨服务 trace 上下文格式。
- **[S7] RFC 9457：** HTTP Problem Details；自然语言 detail 不是机器错误契约。
- **[S8] OWASP Logging Cheat Sheet：** 敏感信息保护、不可信事件输入、日志访问控制和故障测试。

参考地址：
- [S1] <https://opentelemetry.io/docs/specs/otel/logs/data-model/>
- [S2] <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/>
- [S3] <https://docs.rs/tracing/latest/tracing/attr.instrument.html>
- [S4] <https://docs.rs/tracing-appender/latest/tracing_appender/non_blocking/struct.NonBlockingBuilder.html>
- [S5] <https://doc.rust-lang.org/stable/std/panic/fn.set_hook.html>
- [S6] <https://www.w3.org/TR/trace-context/>
- [S7] <https://www.rfc-editor.org/rfc/rfc9457.html>
- [S8] <https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html>

具体库版本由实现仓的当前工具链、依赖锁和 CI 确定。本规范不要求为了追随 latest 文档而擅自升级依赖。
