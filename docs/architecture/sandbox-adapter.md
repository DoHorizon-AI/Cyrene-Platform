# Sandbox Adapter Boundary | SandboxBackend 边界

This document separates the completed API/runtime boundary from deferred
production hardening and future Docker/OCI implementation.

本文区分已经完成的 API/runtime 边界、延期的生产加固，以及未来 Docker/OCI 实现。

## Ownership | 职责归属

The Kernel owns launch authorization, Lease/Fence decisions, lifecycle state,
heartbeat policy, and the generic client for `cyrene.sandbox.v1`. It does not
spawn workers, open cgroup files, read cgroupfs, call pidfd/BPF/prctl, or load a
container-runtime library.

Kernel 拥有启动授权、Lease/Fence 决策、生命周期状态、heartbeat 策略和
`cyrene.sandbox.v1` 的通用 client。它不启动 Worker、不打开 cgroup 文件、不读取
cgroupfs、不调用 pidfd/BPF/prctl，也不加载 container runtime library。

`adapters/execution/sandboxd` is the privileged Adapter Host. It owns the
delegated cgroup subtree, native process start/stop, device-BPF enforcement,
pidfd/process tracking, OOM evidence, and bounded cleanup. It remains Platform
Core because those are enforcement and lifecycle authorities, even though the
implementation runs out of process.

`adapters/execution/sandboxd` 是特权 Adapter Host。它拥有被委托的 cgroup 子树、
native process start/stop、device-BPF 强制、pidfd/进程跟踪、OOM 证据和有界清理。
它虽然进程外运行，仍属于 Platform Core，因为这些能力是强制执行与生命周期权威。

## Port and protocol | 端口与协议

The internal Core port is `SandboxBackend` in
`kernel/crates/cy-kernel-api/src/ports.rs`. It extends `ProcessRuntime`:

内部 Core 端口是 `kernel/crates/cy-kernel-api/src/ports.rs` 中的 `SandboxBackend`。
它继承 `ProcessRuntime`：

| Operation | Meaning | 含义 |
| --- | --- | --- |
| `preflight` | Report host capability and enforcement readiness. | 报告主机能力与强制就绪状态 |
| `launch` | Start the approved plan under the selected backend. | 在选定后端下启动已批准计划 |
| `stop` | Terminate and prove bounded cleanup. | 终止并证明有界清理 |
| `telemetry` | Return backend-owned physical runtime telemetry. | 返回后端拥有的物理运行时 telemetry |
| `discover_recovery_processes` | Enumerate only verifiable backend-owned processes. | 只发现可证明由后端拥有的进程 |
| `recover_stale_process` | Clean only exact persisted evidence matches. | 只清理与持久证据精确匹配的陈旧进程 |

The `cyrene.sandbox.v1` UDS envelope maps these operations to typed request and
response bodies. It is a Core protocol projection, not a worker-controlled
Docker command surface.

`cyrene.sandbox.v1` UDS envelope 将这些操作映射为 typed request/response body。
它是 Core 协议投影，不是 Worker 可控制的 Docker 命令面。

## Native Linux backend | Native Linux 后端

The current backend is `CgroupV2Runtime` with backend ID
`native-cgroup-v2`. Its bounded lifecycle includes cgroup v2 limits, device
BPF for `HARD` bindings when available, parent-death cleanup, pidfd tracking
where available, OOM telemetry, `cgroup.kill`, wait/reap, and exact stale
process evidence checks.

当前后端是 backend ID 为 `native-cgroup-v2` 的 `CgroupV2Runtime`。其有界生命周期
包括 cgroup v2 限制、可用时对 `HARD` binding 使用 device BPF、parent-death 清理、
可用时的 pidfd 跟踪、OOM telemetry、`cgroup.kill`、wait/reap，以及陈旧进程精确证据校验。

The launch path creates and configures the cgroup, forks a child that is held
behind an execution gate, attaches the child to the cgroup, and only then
releases it to `exec`. Therefore the previous user-code spawn-to-attach window
is closed. This does not turn the implementation into a complete hostile-code
sandbox.

启动路径先创建并配置 cgroup，再 fork 一个被 execution gate 暂停的子进程，将子进程
attach 到 cgroup 后才释放它 `exec`。因此旧的用户代码 spawn-to-attach 窗口已经关闭。
这并不意味着实现已经成为完整的恶意代码沙箱。

## Docker/OCI boundary | Docker/OCI 边界

Docker/OCI is a valid future backend boundary, not a current implementation
claim. If selected later, the runtime must own the complete container cgroup
and lifecycle tree, provide a stable runtime handle, stop/reap evidence,
telemetry, and recovery semantics. Native cgroup writes must not also manage a
Docker-owned worker.

Docker/OCI 是有效的未来后端边界，不是当前实现声明。未来选用时，runtime 必须拥有
完整的容器 cgroup 与生命周期树，并提供稳定 runtime handle、stop/reap 证据、telemetry
和 recovery 语义。native cgroup 不能再同时管理由 Docker 拥有的 Worker。

The Kernel must receive a backend-neutral launch plan; image references,
daemon flags, and container-runtime details remain in the selected host profile
or verified installation layer.

Kernel 必须接收 backend-neutral 的 launch plan；image reference、daemon flags 和
container-runtime 细节留在选定的 host profile 或 verified installation 层。

## Security non-guarantees | 安全非保证

Current `native-cgroup-v2` is a bounded cgroup lifecycle sandbox. It does not
provide user/mount/network/PID namespace isolation, seccomp, capability
dropping, complete host-filesystem isolation, or syscall containment. It must
not be marketed as hostile arbitrary-code containment or a complete
multi-tenant security boundary.

当前 `native-cgroup-v2` 是有边界的 cgroup 生命周期沙箱，不提供 user/mount/network/PID
namespace 隔离、seccomp、capability dropping、完整主机文件系统隔离或 syscall containment。
不能把它宣传为恶意任意代码隔离或完整多租户安全边界。

## Status at current HEAD | 当前 HEAD 状态

| Concern | Status | Exact meaning |
| --- | --- | --- |
| `SandboxBackend`/protocol boundary | `COMPLETE` | Core port and `cyrene.sandbox.v1` mapping are present. |
| Native cgroup lifecycle | `COMPLETE` | Current Linux backend owns launch, stop, telemetry, and cleanup. |
| First-user-code enforcement window | `COMPLETE` | Gated fork/attach/exec path is implemented. |
| Hostile arbitrary-code containment | `NOT_COMPLETE` | Required namespace/syscall/capability/filesystem controls are absent. |
| Docker/OCI backend | `DEFERRED` | Boundary is accepted; implementation and real acceptance are absent. |
| Production systemd crash/restart acceptance | `DEFERRED` | Requires the real privileged deployment environment. |

| 范围 | 状态 | 精确含义 |
| --- | --- | --- |
| `SandboxBackend`/协议边界 | `COMPLETE` | Core 端口和 `cyrene.sandbox.v1` 映射存在。 |
| Native cgroup 生命周期 | `COMPLETE` | 当前 Linux 后端拥有启动、停止、telemetry 和清理。 |
| 第一条用户代码的强制窗口 | `COMPLETE` | gated fork/attach/exec 路径已实现。 |
| 恶意任意代码隔离 | `NOT_COMPLETE` | 缺少所需 namespace/syscall/capability/filesystem 控制。 |
| Docker/OCI 后端 | `DEFERRED` | 边界已接受；实现和真实验收尚不存在。 |
| 生产 systemd 崩溃/重启验收 | `DEFERRED` | 需要真实特权部署环境。 |

The Chinese mirror is [`../zh-CN/architecture/sandbox-adapter.md`](../zh-CN/architecture/sandbox-adapter.md).

中文镜像见 [`../zh-CN/architecture/sandbox-adapter.md`](../zh-CN/architecture/sandbox-adapter.md)。
