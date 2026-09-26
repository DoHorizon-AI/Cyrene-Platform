# CYRENE Platform architecture

## Boundary

Platform supplies a generic distributed execution substrate. Products own
application workflows and durable business state. Plugin repositories own
versioned capability payloads, SDKs, runtime launchers, implementations, and
TCKs.

## Layers

### Kernel

The Rust Kernel is the node-local authority for identities, Leases, fences,
resource and lifecycle transitions, and bounded events. It does not contain
global Product policy, AI runtimes, vendor libraries, or business protocols.

### Adapter hosts

Sandbox and hardware adapters run as separately authenticated processes. They
translate privileged or vendor-specific mechanisms into generic resource facts
and actions. Kernel never loads their libraries in-process.

### Framework and agents

Rust framework crates and agents implement generic execution placement,
workspace transport, Artifact transfer, verified Plugin package lifecycle, and
process supervision. A package process publishes an opaque `connection_ref`.
Platform does not handle the endpoint's business payload.

### Products and Plugins

A Product asks Platform to select/start a compatible Plugin, receives the exact
binding and endpoint, then invokes that endpoint using the Plugin-owned client
contract. Product lifecycle state remains in the Product repository.

```text
Product ──control──► Platform ──authority──► Kernel/Adapter
   │                    │
   └──business data────►Plugin endpoint
                        ▲
                  process supervision
```

## Communication planes

- Control plane: identity, admission, lifecycle, health, permissions, and
  endpoint discovery.
- Artifact Plane: content identity, verified transfer, and staging.
- Capability data plane: direct Product-to-Plugin protocol outside Platform.

See `docs/governance/platform-clean-boundary.md` for the extension rule.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Platform 架构

## 边界

Platform 提供通用的分布式执行基础设施。Products 拥有应用工作流和持久业务状态。插件仓库拥有带版本的能力负载、SDK、运行时启动器、实现和 TCK。

## 分层

### Kernel

Rust Kernel 是节点本地权威组件，负责身份、Lease、围栏、资源与生命周期状态转换，以及有界事件。它不包含全局 Product 策略、AI 运行时、厂商库或业务协议。

### 适配器主机

沙箱和硬件适配器以独立且经过认证的进程运行。它们将特权机制或厂商特定机制转换为通用资源事实与操作。Kernel 不会在进程内加载这些适配器库。

### Framework 与代理

Rust framework crate 和 agent 实现通用执行放置、workspace 传输、Artifact 传输、经验证的 Plugin 包生命周期及进程监管。包进程会发布不透明的 connection_ref。Platform 不处理该端点的业务负载。

### Products 与 Plugins

Product 请求 Platform 选择并启动兼容的 Plugin，接收精确的绑定与端点，然后使用 Plugin 所有的客户端契约调用该端点。Product 生命周期状态仍由 Product 仓库管理。

```text
Product ──control──► Platform ──authority──► Kernel/Adapter
   │                    │
   └──business data────►Plugin endpoint
                        ▲
                  process supervision
```

## 通信平面

- 控制面：身份、准入、生命周期、健康状态、权限和端点发现。
- Artifact Plane：内容身份、经验证的传输和暂存。
- 能力数据面：Platform 之外 Product 到 Plugin 的直接协议。

扩展规则见 `docs/governance/platform-clean-boundary.md`。
