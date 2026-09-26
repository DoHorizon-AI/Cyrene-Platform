# Cyrene-Platform Canonical API & Substrate Contract Specification

Cyrene-Platform is the shared execution substrate for Products and capability
plugins. Its public surface is product-neutral: Kernel execution, isolation,
resource observations, capability resolution, Artifact references, and versioned
wire contracts.

## Repository ownership

Platform owns:

- Kernel process execution, sandboxing, resource accounting, Lease/Fence
  enforcement, and worker lifecycle transport;
- canonical Node resource observations and immutable HardwareFacts projections;
- generic capability package resolution and language-neutral worker/service
  runtime selection;
- Artifact identity, CAS mechanisms, and generic contract TCKs.

Products own their run, attempt, retry, workflow, draft, result, and persistence
state. Plugins own capability implementations and conversion between generic
wire objects and capability-specific request types. Adding a Product or plugin
must not require a Platform source change.

The former Python/Kotlin ProductRun control plane moved to Cyrene-Yield. The
former in-Platform model analyzer and compatibility evaluator moved behind
Yield-owned replaceable ports; the official Hugging Face analyzer remains in
Cyrene-Plugins-Official. These are no longer Platform APIs.

## Public Platform objects

| Object / Type | Contract | Status | Responsibility |
|---|---|---|---|
| WorkerControl | kernel_authority.proto | IMPLEMENTED | Generic supervised worker lifecycle. |
| HardwareFacts | cyrene_preflight and Node resource inventory | IMPLEMENTED | Immutable Platform resource projection. |
| ArtifactRef | cyrene_artifacts and manifest schemas | IMPLEMENTED | Immutable digest-addressed artifact reference. |
| CapabilityRequirement | plugin manifest schema | IMPLEMENTED | Capability/interface and execution-mode requirement. |

cyrene_preflight exposes contracts and resource facts. It does not choose model
architecture, estimate model memory, or decide Product admission.

## Execution relationship

A Product resolves a capability requirement, persists its own lifecycle state,
and passes bounded execution intent to the Platform Kernel. The Kernel
supervises the selected plugin worker or service. The plugin returns a typed
result or ArtifactRef to the Product.

Platform never infers a Product run from a process, worker, route, or container
identity.

## Implementation status

| Subsystem | Status | Notes |
|---|---|---|
| Rust Kernel supervision and isolation | IMPLEMENTED | Workspace Cargo gates build all targets. |
| Node resource observation | IMPLEMENTED | Node Agent is the resource-fact authority. |
| Artifact Plane | IMPLEMENTED | Generic CAS and ArtifactRef contracts. |
| Preflight contracts | IMPLEMENTED | Facts and replaceable capability interfaces only. |
| Capability package runtime | IMPLEMENTED | Language-neutral worker/service dispatch. |
| Product lifecycle | EXTERNAL | Owned by each Product repository. |
| Capability implementations | EXTERNAL | Owned by plugin or consuming repositories. |

## 6. Generic Service & Workload Supervision Foundation (`IMPLEMENTED_STABLE`)

The Platform Kernel provides generic, Product-neutral process hosting and supervision mechanisms capable of running long-running service workloads across any runtime (Python, Node.js, JVM, .NET, Rust, Go, or native C/C++ binaries).

> **Architectural Invariant ([ADR-010](adr/ADR-010-service-is-supervisor-orchestration-not-kernel-authority.md))**: `ServiceSpec`, `ServiceState`, `ServiceStatus`, and `ServiceSupervisor` are daemon/orchestration layer constructs. They drive existing Kernel primitives (`LaunchPlan`, `ProcessRuntime`, `SandboxBackend`, `CleanupReport`, and `semantic::Endpoint`). They do **NOT** create a new authoritative Kernel domain entity (no authoritative `ServiceId` resource, no Service ledger/table, and no independent Service repository).

```mermaid
stateDiagram-v2
    [*] --> Starting: spawn_process(LaunchPlan)
    Starting --> Ready: ReadinessProbe Passed (HTTP/TCP/Alive/WorkerControl)
    Starting --> Failed: Probe Timeout / Spawn Error
    Ready --> Running: Publish Endpoint
    Running --> Stopping: Stop / Cancel Requested
    Running --> Restarting: Unexpected Crash (RestartPolicy::OnFailure / Always)
    Restarting --> Starting: Deterministic Backoff Elapsed
    Restarting --> Quarantined: Max Retries Exhausted
    Stopping --> Stopped: Graceful Exit / Clean Stop
    Stopping --> Failed: Forced Kill on Deadline Timeout
    Failed --> [*]
    Stopped --> [*]
    Quarantined --> [*]
```

### A. State Machine & Exact Lifecycle Semantics
- **`Starting`**: Process has been launched into the sandbox/OS runtime and the supervisor is actively evaluating configured readiness probes. The service is **not yet ready** to receive consumer traffic; no endpoint is published.
- **`Ready`**: All configured readiness probes have succeeded (e.g. TCP port open, HTTP 2xx returned, or WorkerControl handshake completed). The semantic `Endpoint` is published to the registry.
- **`Running`**: The service workload is actively running and continuously supervised for liveness, crashes, and exit events.
- **`Stopping`**: Graceful shutdown has been initiated. Published endpoints are revoked immediately. The supervisor waits for the process to exit cleanly within `graceful_stop_timeout`.
- **`Stopped`**: Process has terminated cleanly (exit code 0 or successful graceful stop). This is a terminal resting state.
- **`Failed`**: Process spawn failed, readiness probe timed out, or process required forced termination after shutdown deadline expired.
- **`Restarting`**: An unexpected crash occurred under an active restart policy (`OnFailure` or `Always`). The supervisor is actively counting restart attempts and holding the deterministic backoff delay before spawning the next generation.
- **`Quarantined`**: The consecutive restart ceiling (`max_retries`) has been exhausted. Automated restarts are suspended to prevent infinite crash loops, and the service remains in isolation for investigation.

### B. Core Declarative Types (`cy-kernel-api::service`)
- **`LaunchPlan`**: Contains `executable`, `args`, `environment`, `cgroup_name`, `limits`, `working_dir: Option<PathBuf>`, and optional `transport_socket`.
- **`ReadinessProbe`**:
  - `ProcessAlive`: **Permissive shallow readiness strategy**. Confirms only that the OS/sandbox process was spawned and is alive. It does **NOT** imply application-level, framework-level, or network-level readiness.
  - `TcpSocket { host, port }`: Validates TCP connectivity to the service listening port.
  - `HttpGet { host, port, path, expected_status }`: Validates HTTP endpoint response (e.g. `GET /health/ready` $\rightarrow$ 200 OK).
  - `WorkerControl`: Validates bidirectional framing handshake (`Hello` / `HelloAck`).
- **`RestartPolicy`**:
  - `Never`: Terminal on any exit.
  - `OnFailure { max_retries, backoff }`: Automatically restarts crashed processes with bounded exponential backoff. Clean exits (code 0) transition to `Stopped`.
  - `Always { max_retries, backoff }`: Restarts on both clean exits and unexpected crashes.
- **`BackoffConfig`**: `initial_delay`, `max_delay`, `multiplier`, and `reset_after` for resetting retry counts after sustained healthy execution.
- **`ServiceEndpointSpec`**: Declares transport (e.g. `http`), schema ID, port, path, and public attributes published to the Kernel registry upon reaching `Ready`.

### C. Lifecycle Invariants, Restart Ownership & Failure Semantics
1. **Single Restart Ownership**: Exactly one component (`ServiceSupervisor`) owns restart policies, attempt counters, and backoff timing across generations. Physical execution actors (`InstanceActor` / `SandboxedProcess`) detect and report single-generation crash/exit facts upward; they do not perform restarts.
2. **Stale Endpoint Protection Across Generations**: Endpoints are strictly scoped to the active generation ($N$). When generation $N$ crashes or stops, its endpoint is immediately revoked. During backoff (`Restarting`) and `Starting`, no endpoint is exposed. When generation $N+1$ passes readiness checks, a new endpoint with generation $N+1$ is published; old generation $N$ endpoints are never re-exposed.
3. **Graceful Shutdown & Escalation**: On stop request, processes receive graceful termination. If the process does not terminate within `graceful_stop_timeout`, forced termination is executed and reported via `CleanupReport`.
4. **Deterministic Backoff**: Backoff delays follow $D_n = \min(D_{\text{initial}} \cdot \text{multiplier}^{n-1}, D_{\text{max}})$.
5. **Retry Exhaustion & Quarantine**: Exceeding `max_retries` transitions the service to `Quarantined`, isolating crashing workloads without endless crash-loops.
6. **Zero Zombie / Orphan Guarantee**: All child processes and cgroup allocations are tracked and cleaned up on termination.
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene-Platform 规范 API 与基础设施契约

Cyrene-Platform 是 Products 与 capability plugin 共用的执行基础设施。其公开接口与 Product 无关，涵盖 Kernel 执行、隔离、资源观测、capability 解析、Artifact 引用和版本化 wire 契约。

## 仓库所有权

Platform 拥有：

- Kernel 进程执行、sandbox、资源核算、Lease/Fence 强制执行以及 worker 生命周期传输；
- 规范 Node 资源观测与不可变 HardwareFacts 投影；
- 通用 capability package 解析和语言无关的 worker/service runtime 选择；
- Artifact 身份、CAS 机制与通用契约 TCK。

Products 拥有自身的 run、attempt、retry、workflow、draft、result 与持久化状态。Plugins 拥有 capability 实现，以及通用 wire 对象到专属请求类型的转换。增加 Product 或 Plugin 不应要求修改 Platform 源码。

原 Python/Kotlin ProductRun 控制平面已迁移到 Cyrene-Yield。原来位于 Platform 内的模型分析器与兼容性评估器已迁移到 Yield 所有的可替换端口之后；官方 Hugging Face analyzer 仍由 Cyrene-Plugins-Official 管理。它们不再是 Platform API。

## Platform 公开对象

| 对象 / 类型 | 契约 | 状态 | 职责 |
|---|---|---|---|
| WorkerControl | kernel_authority.proto | IMPLEMENTED | 通用受监管 worker 生命周期。 |
| HardwareFacts | cyrene_preflight 与 Node 资源清单 | IMPLEMENTED | 不可变的 Platform 资源投影。 |
| ArtifactRef | cyrene_artifacts 与 manifest schema | IMPLEMENTED | 不可变、按摘要寻址的制品引用。 |
| CapabilityRequirement | plugin manifest schema | IMPLEMENTED | capability/interface 与执行模式需求。 |

cyrene_preflight 暴露契约与资源事实。它不选择模型架构、不估算模型内存，也不决定 Product 准入。

## 执行关系

Product 解析 capability 需求，持久化自身生命周期状态，并将有界执行意图传递给 Platform Kernel。Kernel 监管所选 Plugin worker 或 service。Plugin 将有类型的结果或 ArtifactRef 返回给 Product。

Platform 不会根据进程、worker、route 或 container 身份推断 Product run。

## 实现状态

| 子系统 | 状态 | 说明 |
|---|---|---|
| Rust Kernel 监管与隔离 | IMPLEMENTED | Workspace Cargo 门禁构建所有 target。 |
| Node 资源观测 | IMPLEMENTED | Node Agent 是资源事实权威。 |
| Artifact Plane | IMPLEMENTED | 通用 CAS 与 ArtifactRef 契约。 |
| Preflight 契约 | IMPLEMENTED | 仅提供事实与可替换 capability 接口。 |
| Capability package runtime | IMPLEMENTED | 语言无关的 worker/service 分发。 |
| Product 生命周期 | EXTERNAL | 由各 Product 仓库拥有。 |
| Capability 实现 | EXTERNAL | 由 plugin 或消费方仓库拥有。 |

## 6. 通用 Service 与工作负载监管基础（IMPLEMENTED_STABLE）

Platform Kernel 提供通用、与 Product 无关的进程托管与监管机制，可在任意 runtime（Python、Node.js、JVM、.NET、Rust、Go 或原生 C/C++ 二进制）上运行长驻 service 工作负载。

> **架构不变量（ADR-010）**：ServiceSpec、ServiceState、ServiceStatus 和 ServiceSupervisor 属于 daemon/编排层结构。它们驱动已有 Kernel 基元（LaunchPlan、ProcessRuntime、SandboxBackend、CleanupReport 与 semantic::Endpoint），不会创建新的 Kernel 权威领域实体（不存在权威 ServiceId 资源、Service ledger/table 或独立 Service repository）。

```mermaid
stateDiagram-v2
    [*] --> Starting: spawn_process(LaunchPlan)
    Starting --> Ready: 就绪探测通过（HTTP/TCP/Alive/WorkerControl）
    Starting --> Failed: 探测超时 / 启动错误
    Ready --> Running: 发布 Endpoint
    Running --> Stopping: 收到 Stop / Cancel 请求
    Running --> Restarting: 非预期崩溃（RestartPolicy::OnFailure / Always）
    Restarting --> Starting: 确定性退避结束
    Restarting --> Quarantined: 重试次数耗尽
    Stopping --> Stopped: 正常退出 / 干净停止
    Stopping --> Failed: 截止时间超时后强制终止
    Failed --> [*]
    Stopped --> [*]
    Quarantined --> [*]
```

### A. 状态机与精确生命周期语义

- **Starting**：进程已启动到 sandbox/OS runtime 中，supervisor 正在执行配置的就绪探测。service 尚未就绪，不能接收消费者流量，也不会发布 endpoint。
- **Ready**：所有配置的就绪探测都成功（例如 TCP 端口已打开、HTTP 返回 2xx 或 WorkerControl handshake 完成）。语义 Endpoint 被发布到 registry。
- **Running**：service 工作负载正在运行，并持续接受存活状态、崩溃和退出事件监管。
- **Stopping**：已发起正常关闭。已发布 endpoint 会立即撤销。supervisor 在 graceful_stop_timeout 内等待进程干净退出。
- **Stopped**：进程已干净终止（退出码为 0 或正常停止成功）。这是终态静止状态。
- **Failed**：进程启动失败、就绪探测超时，或关闭期限过后仍需强制终止进程。
- **Restarting**：在有效的重启策略（OnFailure 或 Always）下发生意外崩溃。supervisor 正在累计重启次数，并等待确定性退避时间后启动下一代。
- **Quarantined**：连续重启上限 max_retries 已耗尽。自动重启暂停以避免无限崩溃循环，service 保持隔离以便调查。

### B. 核心声明式类型（cy-kernel-api::service）

- **LaunchPlan**：包含 executable、args、environment、cgroup_name、limits、working_dir: Option<PathBuf> 和可选的 transport_socket。
- **ReadinessProbe**：
  - ProcessAlive：**宽松的浅层就绪策略**。只确认 OS/sandbox 进程已启动且仍存活，**不**表示应用层、framework 层或网络层已就绪。
  - TcpSocket { host, port }：验证 service 监听端口的 TCP 连通性。
  - HttpGet { host, port, path, expected_status }：验证 HTTP endpoint 响应（例如 GET /health/ready 返回 200 OK）。
  - WorkerControl：验证双向 framing handshake（Hello / HelloAck）。
- **RestartPolicy**：
  - Never：任何退出都进入终态。
  - OnFailure { max_retries, backoff }：使用有界指数退避自动重启崩溃进程。干净退出（退出码 0）进入 Stopped。
  - Always { max_retries, backoff }：干净退出和意外崩溃都会触发重启。
- **BackoffConfig**：通过 initial_delay、max_delay、multiplier 和 reset_after 配置退避，以及健康运行一段时间后重置重试计数。
- **ServiceEndpointSpec**：声明传输方式（如 http）、schema ID、端口、路径以及 service 进入 Ready 后发布到 Kernel registry 的公开属性。

### C. 生命周期不变量、重启归属与失败语义

1. **单一重启所有者**：每一代的重启策略、尝试计数器和退避时间只能由一个组件 ServiceSupervisor 管理。实际执行 actor（InstanceActor / SandboxedProcess）只检测并上报单代崩溃/退出事实，不负责重启。
2. **跨代 endpoint 防陈旧**：Endpoint 严格绑定到活动 generation N。第 N 代崩溃或停止时，其 endpoint 立即撤销。在退避（Restarting）与启动（Starting）期间不暴露 endpoint。第 N+1 代通过就绪检查后，发布绑定到 N+1 的新 endpoint；绝不重新暴露旧的第 N 代 endpoint。
3. **正常关闭与升级处置**：收到 stop 请求后向进程发送正常终止信号。如果进程在 graceful_stop_timeout 内未终止，则执行强制终止，并通过 CleanupReport 报告。
4. **确定性退避**：退避延迟遵循 D_n = min(D_initial × multiplier^(n-1), D_max)。
5. **重试耗尽与隔离**：超过 max_retries 后，service 进入 Quarantined，将持续崩溃的工作负载隔离，避免无休止地崩溃重启。
6. **零僵尸 / 孤儿保证**：终止时跟踪并清理所有子进程与 cgroup 分配。
