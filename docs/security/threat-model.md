# Platform trust and privilege model

This document describes the current code boundary. It is an architecture and
deployment aid, not a promise that the resource sandbox contains hostile code.

## Components and trust boundaries

| Component | Current role | Boundary and authority |
| --- | --- | --- |
| Kernel daemon | Platform Core authority | Owns principals, leases, fence tokens, lifecycle decisions, and generic events. It does not own vendor discovery or cgroup files. |
| `cyrene-sandboxd` | Privileged Platform Core service | Runs out of process, but remains Core because it creates and cleans owned cgroups, applies device BPF policy, starts/stops workers, and reports runtime evidence. |
| Node agent | Platform agent | Exchanges typed control messages with Kernel; does not become lease authority. |
| Runtime agent | Platform agent | Reports runtime state and progress through the typed protocol; does not become a second lifecycle authority. |
| Provider / hardware Adapter | External extension or reference implementation | Runs behind the versioned hardware UDS protocol and reports resource facts/bindings; its implementation is not linked into the Kernel. |
| Worker / service plugin | External extension | Runs as a Kernel-approved child through sandboxd and exchanges opaque or versioned worker messages. Product behavior remains outside Platform. |

An out-of-process boundary is therefore not automatically a licensing or
trust boundary: sandboxd is an example of a privileged Core service.

## Authentication and transport

* Kernel-to-sandboxd and Kernel-to-hardware-Adapter traffic uses bounded,
  versioned Unix-domain-socket frames. Socket mode/ownership is part of the
  deployment boundary.
* sandboxd and NVIDIA Adapter can require the Kernel's UID/GID using
  `SO_PEERCRED`. The Kernel can require the reverse peer identity for each
  configured endpoint. Linux system Adapter peer flags are currently optional;
  without them, access relies on protected socket permissions.
* Workspace relay connections use TLS certificates, a configured server name,
  and the relay protocol's authenticated hello/session flow.
* Worker payloads are not a Rust trait-object plugin ABI. The Kernel's
  `InstanceActor` transport hook preserves correlation, generation, fence,
  timeout, and cancellation; external workers use the documented wire seam.

## Privilege and enforcement

The sandbox service needs access to its delegated cgroup subtree and, for hard
device policy, the Linux BPF/device-control capability required by the host.
A production deployment must grant sandboxd and any NVIDIA sidecar only the
privileges required by those operations, configure the explicit UDS peer
settings, and keep service sockets inaccessible to untrusted workers.

The implementation currently provides resource accounting/capping, process
ownership and supervision, device visibility/enforcement policy where the
configured backend supports it, pidfd tracking where available, and
fence/lifecycle integrity. These are mechanism guarantees, not a general
arbitrary-code security boundary.

## Non-guarantees and evidence status

Source inspection of the current sandboxd shows cgroup v2 controls,
`cgroup.kill`, pidfd support, parent-death signalling, and cgroup-device BPF.
It does not show user/mount/network namespace setup, seccomp installation, or
capability dropping. Full systemd crash/restart and cross-host mTLS acceptance
remain deployment-level tests; they are not inferred from unit tests or a
successful compilation.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 信任与特权模型

本文描述当前代码边界，可作为架构与部署参考，但不保证资源沙箱能够隔离恶意代码。

## 组件与信任边界

| 组件 | 当前角色 | 边界与权威 |
| --- | --- | --- |
| Kernel daemon | Platform Core 权威组件 | 拥有 principals、leases、围栏令牌、生命周期决策和通用事件；不负责厂商探测或 cgroup 文件。 |
| `cyrene-sandboxd` | 特权 Platform Core 服务 | 虽在进程外运行，仍属于 Core，因为它创建并清理自有 cgroup、应用设备 BPF 策略、启动/停止 worker 并报告运行证据。 |
| Node agent | Platform agent | 与 Kernel 交换类型化控制消息；不会成为 lease authority。 |
| Runtime agent | Platform agent | 通过类型化协议报告运行状态与进度；不会成为第二个生命周期权威。 |
| Provider / hardware Adapter | 外部扩展或参考实现 | 通过版本化硬件 UDS 协议运行，并报告资源事实/绑定；其实现不会链接进 Kernel。 |
| Worker / service plugin | 外部扩展 | 作为经 Kernel 批准、由 sandboxd 启动的子进程运行，并交换不透明或版本化的 worker 消息。Product 行为留在 Platform 之外。 |

因此，进程外边界本身并不会自动形成许可或信任边界：sandboxd 就是一个特权 Core 服务的例子。

## 认证与传输

- Kernel 与 sandboxd、Kernel 与硬件 Adapter 之间的通信使用有界、版本化的 Unix domain socket 帧。Socket 模式和所有权属于部署边界的一部分。
- sandboxd 和 NVIDIA Adapter 可以使用 `SO_PEERCRED` 校验 Kernel 的 UID/GID。Kernel 也可以针对各个已配置端点反向校验对端身份。Linux system Adapter 的对端校验开关当前是可选的；未启用时，访问依赖受保护的 socket 权限。
- Workspace relay 连接使用 TLS 证书、配置的服务器名称，以及 relay 协议中经过认证的 hello/session 流程。
- Worker 负载不是 Rust trait object 插件 ABI。Kernel 的 `InstanceActor` transport hook 保留关联关系、generation、fence、timeout 与 cancellation；外部 worker 使用文档规定的线协议边界。

## 特权与强制执行

沙箱服务需要访问委派给它的 cgroup 子树；如果要实施严格的设备策略，还需要主机提供 Linux BPF/device-control 所需能力。生产部署必须只授予 sandboxd 和任何 NVIDIA sidecar 执行这些操作所需的权限，配置明确的 UDS 对端设置，并确保不受信任的 worker 无法访问服务 socket。

当前实现提供资源记账/限额、进程所有权与监管、在已配置后端支持时的设备可见性/强制策略、可用时的 pidfd 跟踪，以及 fence/生命周期完整性。这些是机制保证，不构成通用的任意代码安全边界。

## 不作保证的事项与证据状态

对当前 sandboxd 的源码检查显示已使用 cgroup v2 控制、`cgroup.kill`、pidfd 支持、父进程死亡信号以及 cgroup-device BPF；没有看到设置 user/mount/network namespace、安装 seccomp 或丢弃 capabilities 的实现。完整的 systemd 崩溃/重启和跨主机 mTLS 验收仍属于部署级测试；不能从单元测试或成功编译推断这些能力已获验证。
