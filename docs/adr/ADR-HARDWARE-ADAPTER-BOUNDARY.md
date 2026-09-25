# ADR-HARDWARE-ADAPTER-BOUNDARY: External Hardware Adapter Hosts

- Status: Accepted / Normative
- Date: 2026-08-10
- Supersedes: the in-process vendor-adapter exception in earlier architecture notes

## Decision

The CYRENE Kernel is a minimal user-space microkernel. It owns only node-local
resource leases and fencing, lifecycle decisions, and generic clients for
local adapter IPC. cgroup/process isolation and reaping belong to the external
Sandbox Adapter Host. The Kernel does not execute vendor commands,
read vendor-specific sysfs/procfs paths, enumerate vendor device nodes, load a
vendor shared object, or export a vendor C ABI.

Each vendor integration is an independently supervised Adapter Host process.
`adapters/hardware/nvidia` is the current deployment example; AMD, Ascend,
Intel, virtual partitioners, and private accelerator integrations are peers,
not branches inside a generic Kernel discovery crate. Kernel and host use the
versioned `cyrene.hardware.v1` Protobuf request/response protocol over a
bounded Unix domain socket frame. The Kernel-side implementation is the
vendor-neutral `cy-adapter-client` crate.

The Kernel is configured with one or more explicit `adapter_id=absolute-uds`
endpoints. It does not infer a vendor from the adapter ID, auto-discover a
sidecar, or contain a vendor fallback. The adapter client verifies the response
identity, records the configured adapter ID as every device's provenance,
creates a Kernel-local aggregate inventory generation, and routes a device
binding only back to its provenance adapter. Duplicate physical device IDs
across sidecars fail closed rather than selecting an arbitrary owner.

Vendor C ABI is allowed only *inside* an Adapter Host (or a private dynamic
library loaded by it). It is an internal implementation choice, not a CYRENE
public API and not a substitute for process isolation.

## Failure and ownership rules

- Adapter loss yields typed `ADAPTER_UNAVAILABLE` / `DEGRADED` evidence and
  blocks new leases requiring that adapter; it does not by itself kill existing
  workloads.
- Adapter inventory is a versioned, expiring fact. The resource manager keeps
  the lease/fencing authority and rejects stale generations. The registry
  aggregates independently versioned sidecar facts into its own monotonic
  Kernel generation; it never compares vendor-local generations directly.
- Adapter Hosts must run under their own service manager scope. A Kernel crash
  cannot take an adapter down through shared address space, and an adapter
  crash cannot unwind the Kernel.
- Sandbox Adapter Host pre-flight cleanup may remove only cgroups and runtime
  paths it can prove it owns. Prefix-wide deletion and adoption of foreign
  processes are prohibited.
- Worker health requires IPC heartbeats plus deadline policy; PID liveness,
  cgroup telemetry, and a successful old inventory sample are insufficient.

## Consequences

This adds one local IPC hop and a small protocol mapping layer. In return,
driver failures, blocking vendor calls, and FFI memory corruption stop at the
Adapter Host boundary. The Kernel remains language-neutral at its externally
visible boundary: Kotlin, Python, Rust, C/C++, and future services consume
versioned contracts rather than a Rust or vendor-specific ABI.

## Acceptance gates

- no vendor command, vendor shared-library loading, or vendor device-path scan
  exists under `kernel/`;
- Kernel startup accepts multiple explicit adapter endpoints and has no
  vendor-specific endpoint, service dependency, or fallback path;
- a device binding is routed only to the adapter that supplied its provenance,
  and duplicate device IDs across adapters fail closed;
- a disconnected adapter cannot produce allocatable inventory or a new lease;
- malformed or oversized UDS frames fail closed without panicking the Kernel;
- adapter protocol compatibility and stale-generation rejection are tested;
- adapter services have independent lifecycle supervision and logs.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-HARDWARE-ADAPTER-BOUNDARY：进程外 Hardware Adapter Host

- 状态：Accepted / Normative
- 日期：2026-08-10
- 取代内容：早期架构说明中允许在进程内运行 vendor adapter 的例外

## 决策

CYRENE Kernel 是最小化的用户态 microkernel。它只拥有节点本地资源 Lease 与 fencing、生命周期决策，以及本地 adapter IPC 的通用 client。cgroup/进程隔离和回收属于进程外 Sandbox Adapter Host。Kernel 不运行厂商命令、不读取厂商专属 sysfs/procfs 路径、不枚举厂商设备节点、不加载厂商 shared object，也不导出厂商 C ABI。

每项厂商集成都由一个独立监管的 Adapter Host 进程承载。`adapters/hardware/nvidia` 是当前部署示例；AMD、Ascend、Intel、虚拟分区器和私有加速器集成都与它并列，而不是某个通用 Kernel discovery crate 内的分支。Kernel 与 host 使用版本化 `cyrene.hardware.v1` Protobuf request/response 协议，通过有界 Unix domain socket frame 通信。Kernel 侧实现是与厂商无关的 `cy-adapter-client` crate。

Kernel 使用一个或多个显式 `adapter_id=absolute-uds` endpoint 配置。它不会根据 adapter ID 推断厂商、自动发现 sidecar 或设置厂商 fallback。Adapter client 会验证 response identity，将配置中的 adapter ID 记录为每个设备的 provenance，创建 Kernel 本地的 aggregate inventory generation，并只将 device binding 路由回其 provenance adapter。不同 sidecar 报告相同物理 device ID 时，必须失败关闭，不能任意选择 owner。

厂商 C ABI 只允许存在于 Adapter Host 内部（或由它加载的私有动态库内）。这是实现选择，不是 CYRENE public API，也不能替代进程隔离。

## 故障与归属规则

- Adapter 失联时产生类型化的 `ADAPTER_UNAVAILABLE` / `DEGRADED` 证据，并阻止需要该 adapter 的新 Lease；但不会仅因失联而终止已有工作负载。
- Adapter inventory 是版本化、具有过期时间的事实。Resource manager 拥有 Lease/fencing authority，并拒绝过期 generation。Registry 将独立版本化的 sidecar 事实聚合为自身单调递增的 Kernel generation；不会直接比较各厂商本地 generation。
- Adapter Host 必须处于自己的 service manager scope 中。Kernel 崩溃不能通过共享地址空间带倒 Adapter；Adapter 崩溃也不能 unwind Kernel。
- Sandbox Adapter Host 的 pre-flight cleanup 只能删除能证明归自己所有的 cgroup 和 runtime 路径。禁止按前缀批量删除，也禁止接管外部进程。
- Worker 健康状态需要 IPC heartbeat 和 deadline 策略；PID 存活、cgroup 遥测以及先前成功的 inventory sample 都不足以证明健康。

## 后果

这会增加一跳本地 IPC 和一个小型协议映射层。相应地，driver 故障、厂商调用阻塞和 FFI 内存损坏会被限制在 Adapter Host 边界内。Kernel 对外边界保持语言无关：Kotlin、Python、Rust、C/C++ 和未来 Service 都消费版本化契约，而非 Rust 或厂商专属 ABI。

## 验收 Gate

- `kernel/` 下不存在厂商命令、厂商 shared library 加载或厂商设备路径扫描；
- Kernel 启动接受多个显式 adapter endpoint，且无厂商专属 endpoint、Service 依赖或 fallback 路径；
- device binding 只路由到提供该 provenance 的 adapter，跨 adapter 出现重复 device ID 时失败关闭；
- Adapter 断开后不能提供可分配 inventory 或新 Lease；
- 格式错误或过大的 UDS frame 必须失败关闭，且不能 panic Kernel；
- 测试 adapter 协议兼容性和对过期 generation 的拒绝；
- Adapter Service 有独立的生命周期监管和日志。
