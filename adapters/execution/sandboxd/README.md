# Sandbox Adapter Host | 沙箱适配器主机

`cyrene-sandboxd` is the privileged Platform Core service that implements the
native Linux cgroup v2 `SandboxBackend`. It is out of process for safety and
failure containment, but it is not an independently licensed third-party
plugin: it owns enforcement, process lifecycle, and cleanup authority.

`cyrene-sandboxd` 是 Platform Core 的特权服务，实现 Linux native cgroup v2
`SandboxBackend`。它为了安全与故障隔离而进程外运行，但不是独立授权的第三方
插件：它拥有强制执行、进程生命周期和清理权威。

## Current backend | 当前后端

The shipped backend is `native-cgroup-v2`. Its contract accepts preflight,
launch, stop, telemetry, recovery discovery, and stale-process recovery
requests over the versioned `cyrene.sandbox.v1` UDS protocol.

当前随仓库交付的后端是 `native-cgroup-v2`。它通过版本化的
`cyrene.sandbox.v1` UDS 协议接受 preflight、launch、stop、telemetry、恢复发现和
陈旧进程恢复请求。

The Linux implementation provides cgroup v2 limits, device-BPF enforcement for
`HARD` bindings when available, pidfd tracking where available, parent-death
cleanup, bounded process-tree cleanup, and OOM telemetry. The launch gate forks
the child before attaching it and does not release the child to `exec` until
attachment succeeds, closing the old spawn-then-attach user-code window.

Linux 实现提供 cgroup v2 限制、可用时对 `HARD` binding 使用 device-BPF 强制、可用时
使用 pidfd 跟踪、parent-death 清理、有界进程树清理和 OOM telemetry。启动 gate 会先
fork 子进程，完成 attach 后才允许子进程 `exec`，从而关闭旧的 spawn-then-attach 用户
代码执行窗口。

## Boundary | 边界

Kernel owns launch authorization, leases, fencing, lifecycle decisions, and
the generic sandbox client. `sandboxd` owns the delegated cgroup subtree,
process operations, device enforcement, and cleanup evidence. Exactly one
backend owns a worker's cgroup and lifecycle tree.

Kernel 拥有启动授权、租约、fencing、生命周期决策和通用 sandbox client。
`sandboxd` 拥有被委托的 cgroup 子树、进程操作、设备强制和清理证据。每个 Worker
只能由一个后端拥有其 cgroup 与生命周期树。

The `SandboxBackend` port is a Core host port, not a public plugin ABI. Docker/
OCI is a supported future adapter boundary: a future backend may let Docker or
another OCI runtime own the container lifecycle, but it must project a stable
runtime handle, stop/reap evidence, telemetry, and recovery semantics. The
current repository does not ship a production Docker sandbox backend.

`SandboxBackend` 是 Core host port，不是第三方公开插件 ABI。Docker/OCI 是已支持的
未来适配器边界：未来后端可以让 Docker 或其他 OCI runtime 拥有容器生命周期，但必须
提供稳定 runtime handle、stop/reap 证据、telemetry 与 recovery 语义。当前仓库尚未
交付生产可用的 Docker sandbox backend。

## Security boundary | 安全边界

This is a bounded cgroup lifecycle sandbox, not hostile arbitrary-code
containment. The current implementation does not claim user/mount/network/PID
namespace isolation, seccomp, capability dropping, complete host-filesystem
isolation, or syscall containment. `HARD` device enforcement fails closed when
the required BPF capability is unavailable.

这是一个有边界的 cgroup 生命周期沙箱，不是恶意任意代码隔离。当前实现不声称提供
user/mount/network/PID namespace 隔离、seccomp、capability dropping、完整主机文件系统
隔离或 syscall containment。需要 BPF 但能力不可用时，`HARD` 设备强制会 fail closed。

## Status | 当前状态

| Area | Status | Evidence / boundary |
| --- | --- | --- |
| Native cgroup lifecycle | `COMPLETE` | `CgroupV2Runtime` and sandbox protocol implementation. |
| Launch-to-cgroup enforcement | `COMPLETE` | Gated fork/attach/exec path. |
| Hostile-code containment | `NOT_COMPLETE` | Namespace, seccomp, capability, filesystem, and syscall controls are absent. |
| Docker/OCI backend | `DEFERRED` | Boundary is documented; backend and real acceptance suite are absent. |
| Production crash/restart acceptance | `DEFERRED` | Real systemd lifecycle validation is environment-required. |

| 范围 | 状态 | 证据 / 边界 |
| --- | --- | --- |
| Native cgroup 生命周期 | `COMPLETE` | `CgroupV2Runtime` 与 sandbox 协议实现。 |
| 启动到 cgroup 的强制窗口 | `COMPLETE` | gated fork/attach/exec 路径。 |
| 恶意代码隔离 | `NOT_COMPLETE` | 缺少 namespace、seccomp、capability、文件系统与 syscall 控制。 |
| Docker/OCI 后端 | `DEFERRED` | 已记录边界，但后端和真实验收套件尚不存在。 |
| 生产崩溃/重启验收 | `DEFERRED` | 真实 systemd 生命周期验证需要环境。 |

See [`docs/architecture/sandbox-adapter.md`](../../../docs/architecture/sandbox-adapter.md)
and [`docs/security/threat-model.md`](../../../docs/security/threat-model.md) for
the normative boundary.

规范边界请阅读 [`docs/architecture/sandbox-adapter.md`](../../../docs/architecture/sandbox-adapter.md)
和 [`docs/security/threat-model.md`](../../../docs/security/threat-model.md)。
---

<!-- Chinese Translation / 中文翻译 -->

# Sandbox Adapter Host | 沙箱适配器主机

cyrene-sandboxd 是 Platform Core 的特权服务，实现 Linux 原生 cgroup v2 SandboxBackend。它为提高安全性与故障隔离能力而运行于进程外，但不是可独立采用其他许可证的第三方 plugin：它拥有 enforcement、process lifecycle 和 cleanup authority。

## 当前 backend

随仓库交付的 backend 是 native-cgroup-v2。它通过有版本的 cyrene.sandbox.v1 UDS protocol 接收 preflight、launch、stop、telemetry、recovery discovery 和 stale-process recovery request。

Linux 实现提供 cgroup v2 limit；在可用时对 HARD binding 执行 device-BPF enforcement、使用 pidfd 跟踪、执行 parent-death cleanup、有界 process-tree cleanup，并提供 OOM telemetry。Launch gate 会先 fork child，再将其 attach 到 cgroup；只有 attach 成功后才允许 child 执行 exec，从而关闭旧的先 spawn 再 attach、期间可能运行用户代码的窗口。

## 边界

Kernel 拥有 launch authorization、Lease、fencing、lifecycle decision 和通用 sandbox client。sandboxd 拥有委派给它的 cgroup subtree、process operation、device enforcement 和 cleanup evidence。每个 Worker 的 cgroup 与 lifecycle tree 只能由一个 backend 拥有。

SandboxBackend port 是 Core host port，不是 public plugin ABI。Docker/OCI 是未来受支持的 adapter boundary：未来 backend 可以由 Docker 或其他 OCI runtime 拥有 container lifecycle，但必须投影稳定的 runtime handle、stop/reap evidence、telemetry 和 recovery semantics。当前仓库没有提供生产级 Docker sandbox backend。

## Security boundary

这是一个有界的 cgroup lifecycle sandbox，不是恶意任意代码 containment。当前实现不声称提供 user/mount/network/PID namespace isolation、seccomp、capability dropping、完整 host-filesystem isolation 或 syscall containment。所需 BPF capability 不可用时，HARD device enforcement 会 fail closed。

## 状态

| 范围 | 状态 | 证据 / 边界 |
|---|---|---|
| Native cgroup lifecycle | COMPLETE | CgroupV2Runtime 与 sandbox protocol 实现。 |
| Launch 到 cgroup 的 enforcement | COMPLETE | gated fork/attach/exec 路径。 |
| 恶意代码 containment | NOT_COMPLETE | 尚无 namespace、seccomp、capability、filesystem 和 syscall control。 |
| Docker/OCI backend | DEFERRED | 已记录 boundary；backend 与真实验收套件尚不存在。 |
| 生产 crash/restart 验收 | DEFERRED | 真实 systemd lifecycle validation 需要相应环境。 |

规范边界见 docs/architecture/sandbox-adapter.md 和 docs/security/threat-model.md。
