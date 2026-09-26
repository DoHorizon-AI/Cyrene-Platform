# ADR-PLUGIN-RUNTIME: CYRENE Local Plugin Runtime Architecture

- **Status:** Superseded / Historical
- **Date:** 2026-07-24
- **Authors:** CYRENE Platform Team
- **Context:** Modularizing CYRENE into a platform architecture with zero-port local process isolation and compile-time Rust extensions.

> This ADR is preserved as historical context. Installable business-plugin
> runtime decisions are superseded by
> [ADR-PLUGIN-EXECUTION-BOUNDARY](ADR-PLUGIN-EXECUTION-BOUNDARY.md).
> In-process Rust remains valid only for audited, statically compiled Core
> platform adapters and is not an installable manifest runtime.

> **Final disposition:** the historical stdio runner, worker SDKs, and Platform
> execution proxy were removed after all current consumers moved to
> Plugins-owned direct endpoints. Platform package lifecycle returns only an
> opaque `connection_ref`; the Product opens it with the Plugin-owned protocol.

---

## 1. Context and Problem Statement

CYRENE is evolving into a modular architecture with a small host and
out-of-process advanced services.
The system requires:
1. Low-latency, high-performance execution on hot paths (e.g., HTTP Gateway, proxy routing).
2. Out-of-process isolation for third-party or Python/JVM logic (e.g., vLLM engines, LLaMA-Factory training, hardware probes) so that runtime failures, dependencies, or memory crashes in plugins never crash the main Rust host.
3. Zero TCP port footprint for local plugins to avoid port exhaustion, permissions issues, and local firewall blocks.
4. Clear versioning, explicit error classification, cancellation, and crash-loop quarantine.

---

## 2. Architectural Decisions

### 2.1 Dual Plugin Execution Models

#### A. Rust In-Process Plugins (`in-proc-rust`)
- **Compilation:** Static linking via Cargo features and explicit composition root (`composition_root.rs`).
- **ABI:** Pure Rust traits (`Plugin`, `Probe`, `ExecutionEngine`, etc.). No C-ABI, `.so`/`.dll` dynamic loading, or unsafe dynamic symbol loading.
- **Use Case:** High-throughput, zero-vtable-overhead hot paths (Gateway auth, rate limiters, local storage).
- **Lifecycle:** Married to binary lifecycle. Changes require binary re-compilation.
- **Open-Core Barrier:** Community builds exclude Pro feature crates at compile-time (`#[cfg(feature = "pro")]`).

#### B. External Subprocess Plugins (`subprocess-python`, `subprocess-jvm`)
- **Isolation:** Executed as standalone OS child processes spawned by Rust `PluginSupervisor`.
- **Transport:** Standard input/output (`stdio`) framing by default.
  - Rust Host writes request frames to child `stdin`.
  - Child writes response/stream frames to `stdout`.
  - `stdout` is strictly reserved for binary protocol messages. Logs MUST be routed to `stderr` or structured log events.
- **Process Boundaries:** Windows Named Pipe and Linux/macOS Unix Domain Sockets (UDS) are supported optional transports, but `stdio` is the mandatory baseline transport.
- **Network Boundaries:** TCP/gRPC is reserved exclusively for external APIs, remote SSH Node Agents, and multi-node clusters. Local out-of-process plugins MUST NOT listen on TCP ports.

---

### 2.2 Protocol Versioning & Host API Compatibility

- **`protocol_version` (uint32, e.g. `1`):** Governs the wire format and framing (`plugin_protocol.proto`). Negotiated during `Hello` / `HelloAck` handshake.
- **`api_version` (string, e.g. `"1.0"`):** Governs domain extension-point semantic contracts.

A host and plugin are compatible iff:
1. `protocol_version` is supported by both.
2. `api_version` satisfies host semver constraints.

---

### 2.3 Extension-Point Invocation Strategies

Each of the 10 extension points follows a strictly defined invocation strategy:

| Extension Point   | Strategy                            | Description                                                                                         |
| ----------------- | ----------------------------------- | --------------------------------------------------------------------------------------------------- |
| `Probe`           | `CollectAndRank` / `FirstAvailable` | Gathers hardware facts from active probes; ranks by evidence confidence.                            |
| `ModelAnalyzer`   | `CollectAndRank`                    | Evaluates model requirements against analyzer capabilities; ranks best estimates.                   |
| `CompatRule`      | `FanOut` + `Merge`                  | Executes rule evaluations concurrently; merges `WhyReport` decision items.                          |
| `RuntimeBuilder`  | `FirstMatch`                        | Selects the first healthy builder plugin covering the target runtime stack.                         |
| `ExecutionEngine` | `FirstMatch`                        | Routes inference requests to the active, healthy engine covering model/precision/quant.             |
| `TrainingBackend` | `FirstMatch`                        | Routes training jobs to the specified backend plugin.                                               |
| `Quantization`    | `FirstMatch`                        | Selects matching quantization engine plugin.                                                        |
| `GatewayFilter`   | `OrderedChain`                      | Executes middleware filters sequentially in configured priority order; short-circuits on rejection. |
| `Notification`    | `FanOut`                            | Broadcasts alert messages to all registered notification plugins.                                   |
| `Storage`         | `NamedSingleOwner`                  | Directs artifact storage operations to the specific declared storage plugin provider.               |

---

### 2.4 Lifecycle, Error Classification & Quarantine

#### Lifecycle States
```text
Discovered -> Resolved -> Starting -> Handshaking -> Healthy -> Degraded -> Stopping -> Stopped
                                                            \-> Unavailable / Incompatible / Crashed / Quarantined / Disabled
```

- **`Healthy`:** Plugin process is running, handshake completed, health check passes.
- **`Quarantined`:** Plugin crashed repeatedly (crash-loop). Supervisor stops restarting until manual intervention or backoff reset.

#### Error Classification
`PluginError` includes 10 typed categories:
1. `Unavailable`: Dependent binaries or services missing.
2. `Incompatible`: Protocol version or API version mismatch.
3. `InvalidInput`: Malformed payload or validation error.
4. `PermissionDenied`: Unauthorized capability access.
5. `Timeout`: Operation exceeded deadline.
6. `Cancelled`: Request canceled by host.
7. `Retryable`: Temporary transient failure.
8. `ExecutionFailed`: Internal plugin logic exception.
9. `ProtocolError`: Framing corruption, invalid stdout content, or JSON/protobuf parse error.
10. `Fatal`: Subprocess crashed or exited abnormally.

#### Subprocess Crash Handling & Restart Policy
- `never`: Process will not be restarted if terminated.
- `on-failure` (default): Restarted with exponential backoff (`min=500ms`, `max=30s`, `factor=2.0`).
- `always`: Always restarted regardless of exit code.
- **Crash Loop Detection:** If a plugin crashes >3 times within a 60-second window, its state transitions to `Quarantined`.

---

## 3. Mandatory Q&A Resolutions

1. **Who launches external plugins?**
   The Rust `PluginSupervisor` component using explicit command/argument arrays (never shell strings).

2. **Who performs version negotiation?**
   The Rust `PluginSupervisor` during initial `Hello` / `HelloAck` RPC handshake over `stdio`.

3. **Are logs allowed on stdout?**
   **No.** `stdout` is strictly reserved for binary/framed protocol messages. Any unformatted non-protocol output on `stdout` causes an immediate `ProtocolError` and quarantine trigger. Logs must go to `stderr`.

4. **What happens when a plugin crashes?**
   The host supervisor catches sub-process EOF / SIGCHLD, flags active requests as `Fatal` / `Unavailable`, and applies the restart policy without crashing the core Rust host.

5. **How are multiple implementations for an extension point chosen?**
   Based on the extension point's invocation strategy (§2.3) combined with declared plugin priority and capability matching.

6. **Which communications are allowed to occupy TCP ports?**
   Only the public OpenAPI REST gateway, gRPC server for external client access, and SSH/gRPC communication to remote Node Agents. Local plugins are strictly zero-port.

---

## 4. Consequences and Compliance

- Core stability is guaranteed against third-party plugin crashes or memory leaks.
- Zero local port allocation removes port conflict risks on developer/production machines.
- Protocol buffer generation tools ensure synchronized cross-language Rust, Python, and JVM SDKs.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-PLUGIN-RUNTIME：CYRENE 本地 Plugin Runtime 架构

- **状态：**Superseded / Historical
- **日期：**2026-07-24
- **作者：**CYRENE Platform Team
- **背景：**将 CYRENE 模块化为具备本地零端口进程隔离和编译期 Rust 扩展的平台架构。

> 本 ADR 保留作历史背景。可安装业务 Plugin runtime 的决策已由 [ADR-PLUGIN-EXECUTION-BOUNDARY](ADR-PLUGIN-EXECUTION-BOUNDARY.md) 取代。进程内 Rust 只适用于经过审计、静态编译的 Core platform adapter，不是可安装 manifest runtime。

> **最终处置：**在当前 consumer 全部迁移到 Plugins-owned direct endpoint 后，旧 stdio runner、worker SDK 和 Platform execution proxy 均已移除。Platform package lifecycle 只返回不透明的 `connection_ref`；Product 使用 Plugin-owned 协议打开该连接。

---

## 1. 背景与问题陈述

CYRENE 正在演进为由精简 host 和进程外高级 Service 组成的模块化架构。系统需要：
1. 在热路径（例如 HTTP Gateway、代理路由）上实现低延迟、高性能执行。
2. 将第三方或 Python/JVM 逻辑进程外隔离（例如 vLLM engine、LLaMA-Factory training、hardware probe），使 Plugin 的 runtime 故障、依赖冲突或内存崩溃都不会导致 Rust 主机崩溃。
3. 本地 Plugin 不使用 TCP 端口，避免端口耗尽、权限问题和本地防火墙阻挡。
4. 明确版本控制、错误分类、取消和 crash-loop quarantine 规则。

---

## 2. 架构决策

### 2.1 两种 Plugin 执行模式

#### A. Rust 进程内 Plugin（`in-proc-rust`）
- **编译方式：**通过 Cargo feature 和显式 composition root（`composition_root.rs`）静态链接。
- **ABI：**使用纯 Rust trait（`Plugin`、`Probe`、`ExecutionEngine` 等）。不使用 C ABI、`.so`/`.dll` 动态加载或不安全的动态符号加载。
- **用途：**高吞吐、热路径，不产生 vtable 开销（Gateway auth、rate limiter、本地存储）。
- **生命周期：**随 binary 生命周期绑定；修改需要重新编译 binary。
- **Open-Core 隔离：**Community build 在编译期排除 Pro feature crate（`#[cfg(feature = "pro")]`）。

#### B. 外部子进程 Plugin（`subprocess-python`、`subprocess-jvm`）
- **隔离方式：**由 Rust `PluginSupervisor` 启动为独立 OS 子进程。
- **传输方式：**默认使用标准输入/输出（`stdio`）分帧：
  - Rust Host 向子进程 `stdin` 写 request frame；
  - 子进程向 `stdout` 写 response/stream frame；
  - `stdout` 只用于二进制协议消息。日志必须写入 `stderr` 或结构化 log event。
- **进程边界：**Windows Named Pipe 和 Linux/macOS Unix Domain Socket（UDS）是可选 transport；`stdio` 是必需的基线传输。
- **网络边界：**TCP/gRPC 仅用于外部 API、远端 SSH Node Agent 和多节点集群。本地进程外 Plugin 绝不能监听 TCP 端口。

---

### 2.2 协议版本与 Host API 兼容性

- **`protocol_version`（uint32，例如 `1`）：**规定 wire format 和 framing（`plugin_protocol.proto`），并在 `Hello` / `HelloAck` 握手中协商。
- **`api_version`（string，例如 `"1.0"`）：**规定领域扩展点的语义契约。

Host 和 Plugin 满足以下条件时才兼容：
1. 双方都支持该 `protocol_version`；
2. `api_version` 满足 Host 的 semver 约束。

---

### 2.3 Extension Point 调用策略

10 个 extension point 均采用严格定义的调用策略：

| Extension Point | 策略 | 说明 |
|---|---|---|
| `Probe` | `CollectAndRank` / `FirstAvailable` | 收集活动 probe 的硬件事实，并按证据可信度排序。 |
| `ModelAnalyzer` | `CollectAndRank` | 根据 analyzer capability 评估模型需求，并将最佳估算排序。 |
| `CompatRule` | `FanOut` + `Merge` | 并发执行规则评估，合并 `WhyReport` decision item。 |
| `RuntimeBuilder` | `FirstMatch` | 选择第一个健康且覆盖目标 runtime stack 的 builder Plugin。 |
| `ExecutionEngine` | `FirstMatch` | 将推理请求路由到覆盖目标 model/precision/quant 的活动健康 engine。 |
| `TrainingBackend` | `FirstMatch` | 将训练任务路由到指定 backend Plugin。 |
| `Quantization` | `FirstMatch` | 选择匹配的 quantization engine Plugin。 |
| `GatewayFilter` | `OrderedChain` | 按配置优先级依次执行 middleware filter；拒绝时短路。 |
| `Notification` | `FanOut` | 向所有已注册 notification Plugin 广播 alert message。 |
| `Storage` | `NamedSingleOwner` | 将 Artifact storage 操作发送到明确声明的 storage Plugin provider。 |

---

### 2.4 生命周期、错误分类与隔离

#### 生命周期状态

```text
Discovered -> Resolved -> Starting -> Handshaking -> Healthy -> Degraded -> Stopping -> Stopped
                                                            \-> Unavailable / Incompatible / Crashed / Quarantined / Disabled
```

- **`Healthy`：**Plugin process 正在运行、握手成功且 health check 通过。
- **`Quarantined`：**Plugin 反复崩溃（crash-loop）；Supervisor 会停止重启，直到人工处理或 backoff reset。

#### 错误分类

`PluginError` 包含 10 种类型：
1. `Unavailable`：依赖的 binary 或 Service 缺失。
2. `Incompatible`：protocol version 或 API version 不匹配。
3. `InvalidInput`：payload 格式错误或校验失败。
4. `PermissionDenied`：无权访问 capability。
5. `Timeout`：Operation 超出 deadline。
6. `Cancelled`：Host 取消 request。
7. `Retryable`：临时性故障，可重试。
8. `ExecutionFailed`：Plugin 内部逻辑异常。
9. `ProtocolError`：frame 损坏、stdout 内容无效或 JSON/protobuf parse 错误。
10. `Fatal`：子进程崩溃或异常退出。

#### 子进程崩溃处理与重启策略
- `never`：进程终止后不重启。
- `on-failure`（默认）：按指数退避重启（`min=500ms`、`max=30s`、`factor=2.0`）。
- `always`：无论 exit code 如何都重启。
- **Crash Loop 检测：**若 Plugin 在 60 秒内崩溃超过 3 次，状态变为 `Quarantined`。

---

## 3. 必须明确的问答

1. **谁启动外部 Plugin？**Rust `PluginSupervisor`，使用明确的 command/argument array，绝不使用 shell string。
2. **谁进行版本协商？**Rust `PluginSupervisor`，在 stdio 上通过初始 `Hello` / `HelloAck` RPC handshake 执行。
3. **允许将日志写入 stdout 吗？****不允许。**`stdout` 严格保留给二进制/分帧协议消息。任何未格式化的非协议输出都会立即引发 `ProtocolError` 和 quarantine。日志必须写入 `stderr`。
4. **Plugin 崩溃时会怎样？**Host supervisor 捕获子进程 EOF / SIGCHLD，将活动 request 标为 `Fatal` / `Unavailable`，并应用重启策略；不会导致 Rust Core host 崩溃。
5. **如何选择同一 extension point 的多个实现？**根据 extension point 的调用策略（§2.3），结合声明的 Plugin 优先级和 capability 匹配结果选择。
6. **哪些通信可以占用 TCP 端口？**仅公开 OpenAPI REST gateway、供外部 client 访问的 gRPC server，以及到远端 Node Agent 的 SSH/gRPC。本地 Plugin 必须完全不占端口。

---

## 4. 后果与合规

- Core 不会因第三方 Plugin 崩溃或内存泄漏而失去稳定性。
- 本地不分配端口，可消除开发和生产机器上的端口冲突风险。
- Protocol buffer 代码生成工具确保 Rust、Python 和 JVM SDK 跨语言同步。
