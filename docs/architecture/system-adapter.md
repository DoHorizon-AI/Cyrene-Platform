# System Adapter Boundary | SystemAdapter 边界

This document is the canonical architecture description for target-specific
host-system adapters. It describes the current Linux implementation without
claiming that every operating system already has an implementation.

本文是目标系统适配器的 canonical 架构说明。它描述当前 Linux 实现，但不声称
所有操作系统都已经有对应实现。

## Purpose | 目的

The `SystemAdapter` is the host-facts and generic host-resource boundary for a
single build target. It allows the Kernel to consume normalized CPU, memory,
NUMA, operating-system capability, binding, and health facts without embedding
Linux `/proc`/`sysfs` parsing or another operating system's APIs in Kernel code.

`SystemAdapter` 是单个构建目标的主机事实与通用主机资源边界。它让 Kernel 消费
标准化的 CPU、内存、NUMA、操作系统能力、binding 和 health 事实，而不把 Linux
`/proc`/`sysfs` 解析或其他操作系统 API 嵌入 Kernel 代码。

The adapter is selected by the target build and deployment profile. It is not
expected that Linux, Windows, macOS, or another implementation runs at the
same time on one node.

适配器由目标构建和部署 profile 选择。一个节点不会同时启用 Linux、Windows、macOS
或其他系统实现。

## Canonical port | 规范端口

The implementation-free port lives in
`contracts/rust/cy-kernel-contract/src/adapter.rs`:

实现无关的端口位于 `contracts/rust/cy-kernel-contract/src/adapter.rs`：

| Port | Responsibility | 职责 |
| --- | --- | --- |
| `HostInventoryProvider::probe_inventory` | Return a generationed inventory snapshot and node capabilities. | 返回带 generation 的 inventory snapshot 与节点能力 |
| `ResourceProvider::adapter_id` | Stable local adapter identity. | 稳定的本地适配器身份 |
| `ResourceProvider::probe_resources` | Return normalized resources owned by this adapter. | 返回本适配器拥有的标准化资源 |
| `ResourceProvider::create_binding` | Project a resource into a Kernel-approved binding. | 将资源投影为 Kernel 批准的 binding |
| `ResourceProvider::read_health` | Return current resource health evidence. | 返回当前资源健康证据 |
| `SystemAdapter::system_id` | Identify the target-system implementation. | 标识目标系统实现 |

`SystemAdapter` extends the host inventory and resource-provider ports. It does
not own leases, fence tokens, lifecycle policy, product state, or vendor GPU
semantics. Those remain Kernel, Framework, or Hardware Adapter concerns.

`SystemAdapter` 继承 host inventory 和 resource-provider 端口。它不拥有租约、fence
token、生命周期策略、产品状态或厂商 GPU 语义；这些仍分别属于 Kernel、Framework
或 Hardware Adapter。

## Linux reference implementation | Linux 参考实现

The current implementation is `LinuxSystemProvider` in
`adapters/hardware/linux_sys/src/probe.rs`, served by the
`cyrene-linux-sys-adapter` binary. It reports:

当前实现是 `adapters/hardware/linux_sys/src/probe.rs` 中的
`LinuxSystemProvider`，由 `cyrene-linux-sys-adapter` 二进制提供服务。它报告：

- `cpu-host`: logical/physical CPU capacity, architecture, model, kernel
  release, NUMA count, and CPU capabilities;
- `ram-host`: total/available memory, swap facts when present, NUMA count, and
  degraded state when available memory is below the implementation threshold;
- node capability and enforcement facts, inventory generation, and health
  responses for the two host resources.

- `cpu-host`：逻辑/物理 CPU 容量、架构、型号、内核版本、NUMA 数量和 CPU 能力；
- `ram-host`：总内存/可用内存、存在时的 swap 事实、NUMA 数量，以及可用内存低于实现
  阈值时的 degraded 状态；
- 节点能力与 enforcement 事实、inventory generation，以及两个主机资源的 health 响应。

CPU and RAM bindings contain no device nodes and report `Soft` enforcement;
resource allocation and cgroup enforcement remain Kernel/sandbox concerns.
NVIDIA discovery, topology, device nodes, and vendor health remain in the
separate NVIDIA Hardware Adapter.

CPU 与 RAM binding 不包含设备节点，并报告 `Soft` enforcement；资源分配和 cgroup
强制仍由 Kernel/sandbox 负责。NVIDIA discovery、topology、设备节点和厂商健康信息
仍属于独立的 NVIDIA Hardware Adapter。

## Transport and deployment | 传输与部署

The Linux host is a separately supervised process using bounded framed
`cyrene.hardware.v1` UDS requests. The Kernel registers its absolute socket and
expected adapter identity; the response identity and inventory provenance are
checked before resources become allocatable.

Linux host 是独立监管的进程，使用有界帧格式的 `cyrene.hardware.v1` UDS 请求。
Kernel 注册绝对 socket 路径与预期适配器身份；在资源可分配前，会校验响应身份与
inventory provenance。

The reference systemd unit is
`infrastructure/systemd/cyrene-linux-sys-adapter.service`. The unit currently
allows peer UID/GID flags to be omitted for compatibility, while production
deployment guidance requires explicit peer identity configuration on both sides.
This is a deployment-hardening status, not a missing semantic port.

参考 systemd unit 是 `infrastructure/systemd/cyrene-linux-sys-adapter.service`。
当前 unit 为兼容性允许省略 peer UID/GID 参数，但生产部署指南要求两侧都显式配置
peer identity。这是部署加固状态，不是语义端口缺失。

## Build profiles | 构建 profile

The public contract is target-neutral; the shipped provider is Linux-only. A
future target provider must implement the same normalized port and protocol
projection, then be selected in that target's build/profile. It must not add
target-specific methods to the Kernel semantic authority.

公共契约与目标系统无关；当前随仓库提供的 provider 仅支持 Linux。未来目标系统的
provider 必须实现同一标准化端口和协议投影，再在该目标的构建/profile 中选择，不能
向 Kernel semantic authority 添加目标系统专属方法。

## Status at current HEAD | 当前 HEAD 状态

| Concern | Status | Exact meaning |
| --- | --- | --- |
| Public `SystemAdapter` port | `COMPLETE` | Defined in the public contract crate and re-exported by the Kernel API. |
| Linux inventory/binding/health | `COMPLETE` | Implemented by `LinuxSystemProvider` and connected through the adapter host. |
| Kernel integration | `COMPLETE` | Linux adapter is an explicit registered endpoint, not in-process discovery. |
| NVIDIA vendor facts | `NOT_APPLICABLE` | Owned by the NVIDIA Hardware Adapter, not this system adapter. |
| Non-Linux providers | `DEFERRED` | No alternate OS implementation is shipped in this repository. |
| Real privileged/systemd deployment | `DEFERRED` | Requires the environment-required acceptance run. |

| 范围 | 状态 | 精确含义 |
| --- | --- | --- |
| 公共 `SystemAdapter` 端口 | `COMPLETE` | 定义在公共 contract crate，并由 Kernel API re-export。 |
| Linux inventory/binding/health | `COMPLETE` | 由 `LinuxSystemProvider` 实现，并通过适配器进程接入。 |
| Kernel 集成 | `COMPLETE` | Linux adapter 是显式注册 endpoint，不是进程内 discovery。 |
| NVIDIA 厂商事实 | `NOT_APPLICABLE` | 属于 NVIDIA Hardware Adapter，不属于 system adapter。 |
| 非 Linux provider | `DEFERRED` | 本仓库尚未交付其他操作系统实现。 |
| 真实特权/systemd 部署 | `DEFERRED` | 需要环境要求的验收运行。 |

The Chinese mirror is [`../zh-CN/architecture/system-adapter.md`](../zh-CN/architecture/system-adapter.md).

中文镜像见 [`../zh-CN/architecture/system-adapter.md`](../zh-CN/architecture/system-adapter.md)。
---

<!-- Chinese Translation / 中文翻译 -->

# SystemAdapter 边界


本文是目标系统适配器的 canonical 架构说明。它描述当前 Linux 实现，但不声称
所有操作系统都已经有对应实现。

## 目的


`SystemAdapter` 是单个构建目标的主机事实与通用主机资源边界。它让 Kernel 消费
标准化的 CPU、内存、NUMA、操作系统能力、binding 和 health 事实，而不把 Linux
`/proc`/`sysfs` 解析或其他操作系统 API 嵌入 Kernel 代码。


适配器由目标构建和部署 profile 选择。一个节点不会同时启用 Linux、Windows、macOS
或其他系统实现。

## 规范端口


实现无关的端口位于 `contracts/rust/cy-kernel-contract/src/adapter.rs`：

| Port | 职责 |
| --- | --- |
| `HostInventoryProvider::probe_inventory` | 返回带 generation 的 inventory snapshot 与节点能力 |
| `ResourceProvider::adapter_id` | 稳定的本地适配器身份 |
| `ResourceProvider::probe_resources` | 返回本适配器拥有的标准化资源 |
| `ResourceProvider::create_binding` | 将资源投影为 Kernel 批准的 binding |
| `ResourceProvider::read_health` | 返回当前资源健康证据 |
| `SystemAdapter::system_id` | 标识目标系统实现 |


`SystemAdapter` 继承 host inventory 和 resource-provider 端口。它不拥有租约、fence
token、生命周期策略、产品状态或厂商 GPU 语义；这些仍分别属于 Kernel、Framework
或 Hardware Adapter。

## Linux 参考实现


当前实现是 `adapters/hardware/linux_sys/src/probe.rs` 中的
`LinuxSystemProvider`，由 `cyrene-linux-sys-adapter` 二进制提供服务。它报告：


- `cpu-host`：逻辑/物理 CPU 容量、架构、型号、内核版本、NUMA 数量和 CPU 能力；
- `ram-host`：总内存/可用内存、存在时的 swap 事实、NUMA 数量，以及可用内存低于实现
  阈值时的 degraded 状态；
- 节点能力与 enforcement 事实、inventory generation，以及两个主机资源的 health 响应。


CPU 与 RAM binding 不包含设备节点，并报告 `Soft` enforcement；资源分配和 cgroup
强制仍由 Kernel/sandbox 负责。NVIDIA discovery、topology、设备节点和厂商健康信息
仍属于独立的 NVIDIA Hardware Adapter。

## 传输与部署


Linux host 是独立监管的进程，使用有界帧格式的 `cyrene.hardware.v1` UDS 请求。
Kernel 注册绝对 socket 路径与预期适配器身份；在资源可分配前，会校验响应身份与
inventory provenance。


参考 systemd unit 是 `infrastructure/systemd/cyrene-linux-sys-adapter.service`。
当前 unit 为兼容性允许省略 peer UID/GID 参数，但生产部署指南要求两侧都显式配置
peer identity。这是部署加固状态，不是语义端口缺失。

## 构建 profile


公共契约与目标系统无关；当前随仓库提供的 provider 仅支持 Linux。未来目标系统的
provider 必须实现同一标准化端口和协议投影，再在该目标的构建/profile 中选择，不能
向 Kernel semantic authority 添加目标系统专属方法。

## 当前 HEAD 状态


| 范围 | 状态 | 精确含义 |
| --- | --- | --- |
| 公共 `SystemAdapter` 端口 | `COMPLETE` | 定义在公共 contract crate，并由 Kernel API re-export。 |
| Linux inventory/binding/health | `COMPLETE` | 由 `LinuxSystemProvider` 实现，并通过适配器进程接入。 |
| Kernel 集成 | `COMPLETE` | Linux adapter 是显式注册 endpoint，不是进程内 discovery。 |
| NVIDIA 厂商事实 | `NOT_APPLICABLE` | 属于 NVIDIA Hardware Adapter，不属于 system adapter。 |
| 非 Linux provider | `DEFERRED` | 本仓库尚未交付其他操作系统实现。 |
| 真实特权/systemd 部署 | `DEFERRED` | 需要环境要求的验收运行。 |


中文镜像见 [`../zh-CN/architecture/system-adapter.md`](../zh-CN/architecture/system-adapter.md)。
