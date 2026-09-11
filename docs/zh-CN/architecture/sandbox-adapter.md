# Sandbox Adapter 边界

英文 canonical source：[docs/architecture/sandbox-adapter.md](../../architecture/sandbox-adapter.md)。

Kernel 拥有启动授权、Lease/Fence、生命周期状态、heartbeat 策略和
`cyrene.sandbox.v1` client；不启动 Worker、不打开 cgroup 文件、不调用 pidfd/BPF/prctl，
也不加载 container runtime library。

`adapters/execution/sandboxd` 是特权 Platform Core Adapter Host，拥有被委托的
cgroup 子树、native process start/stop、device-BPF、pidfd/进程跟踪、OOM 证据和有界
清理。它虽然进程外运行，仍不是第三方 Plugin，因为它拥有强制执行和生命周期权威。

## 端口与协议

Core 内部端口是 `kernel/crates/cy-kernel-api/src/ports.rs` 的 `SandboxBackend`，
其操作包括：

- `preflight`：报告主机能力和 enforcement readiness；
- `launch`：在选择的后端下启动批准的计划；
- `stop`：终止并证明有界清理；
- `telemetry`：返回后端拥有的运行时 telemetry；
- `discover_recovery_processes`：只发现可证明由后端拥有的进程；
- `recover_stale_process`：只清理与持久证据精确匹配的陈旧进程。

`cyrene.sandbox.v1` 是这些操作的 typed UDS projection，不是 Worker 可控的 Docker
命令面。

## 当前 native 后端

`CgroupV2Runtime` 的 backend ID 是 `native-cgroup-v2`。它提供 cgroup v2 limits、
可用时的 `HARD` device-BPF、parent-death cleanup、可用时的 pidfd、OOM telemetry、
`cgroup.kill`、wait/reap 和 stale process evidence 检查。

启动路径先 fork 一个被 gate 暂停的子进程，完成 cgroup attach 后才释放它 `exec`，因此
旧的用户代码 spawn-to-attach 窗口已经关闭。但这仍不是完整恶意代码沙箱。

## Docker/OCI

Docker/OCI 是已接受的未来 Adapter boundary，不是当前实现。未来后端必须拥有完整
容器 cgroup/lifecycle tree，提供稳定 runtime handle、stop/reap evidence、telemetry
和 recovery semantics。native cgroup 不能同时管理 Docker 所拥有的 Worker。

## 安全非保证

当前是 bounded cgroup lifecycle sandbox，不提供 user/mount/network/PID namespace、
seccomp、capability dropping、完整 host filesystem isolation 或 syscall containment。
不能把它宣传为 hostile arbitrary-code containment 或完整 multi-tenant boundary。

## 当前状态

| 范围 | 状态 |
| --- | --- |
| `SandboxBackend`/协议边界 | `COMPLETE` |
| Native cgroup 生命周期 | `COMPLETE` |
| 第一条用户代码的 cgroup 强制窗口 | `COMPLETE` |
| 恶意任意代码隔离 | `NOT_COMPLETE` |
| Docker/OCI 后端 | `DEFERRED` |
| 生产 systemd 崩溃/重启验收 | `DEFERRED` |
