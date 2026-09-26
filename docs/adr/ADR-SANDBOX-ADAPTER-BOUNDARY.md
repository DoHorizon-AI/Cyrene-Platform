# ADR-SANDBOX-ADAPTER-BOUNDARY: External Privileged Sandbox Host

- Status: Accepted / Normative
- Date: 2026-08-11

## Decision

`kernel/` is a pure-safe Rust decision core. It owns leases, fencing, launch
authorization, lifecycle state, heartbeat policy, and the generic client for
the local `cyrene.sandbox.v1` UDS protocol. It does not spawn a worker, open a
cgroup file, read `/proc/self/cgroup`, send a signal, call pidfd/BPF/prctl, or
load a container runtime library.

`adapters/execution/sandboxd` is a separately supervised privileged Adapter
Host. The initial backend is native Linux process plus cgroup v2. It owns the
delegated cgroup subtree, pre-flight cleanup of only its proven `instance-*`
children, limits, device-BPF enforcement, process reaping, OOM counters, and
the SIGTERM -> cgroup.kill -> reap cleanup truth. Its Rust may use narrowly
audited Linux `unsafe` where the OS API makes that unavoidable; that exception
does not cross the UDS boundary into Kernel code.

Hardware Adapter Hosts remain independent: they report device facts and a
binding over `cyrene.hardware.v1`; `sandboxd` consumes the Kernel-approved
binding but never selects a vendor or creates a resource lease. A binding that
requests `HARD` must fail closed if device enforcement cannot be installed.

## System and runtime backends

The host-system facts are exposed through the `SystemAdapter` port. The current
production build selects the Linux system Adapter and native cgroup v2 sandbox
backend; target-specific builds must select one platform implementation at
compile time. Kernel semantic authority is not moved into a system Adapter.

Docker/OCI is a supported Adapter boundary, not a Kernel feature and never a
raw Docker CLI input exposed through Core RPC. The selected profile is host
configuration, not a worker-controlled argument. For each worker exactly one
backend owns the cgroup/lifecycle tree:

- native-cgroup: `sandboxd` creates and reaps the cgroup directly;
- oci/docker: the selected runtime owns the container cgroup and lifecycle;
- systemd scope: systemd owns the scope and lifecycle.

Mixing direct cgroup writes with a Docker-owned worker is prohibited. This
avoids both conflicting limits and ambiguous cleanup ownership. The current
implementation ships and runs only `native-cgroup` on Linux. A Docker/OCI
Adapter may be selected only when it owns the complete runtime reference,
stop/reap path, telemetry projection, and restart evidence for its containers;
it must not reuse the native `cgroup_path` handle as a container identifier.
Until that handle projection and the real Docker acceptance suite are present,
Docker remains an explicit build/runtime extension point rather than a
production-ready sandbox claim.

## Failure and restart ownership

`cyrene-sandboxd.service` receives cgroup delegation and `KillMode=control-group`;
`cyrene-kernel.service` receives neither. systemd orders Kernel after sandboxd
and makes sandboxd `PartOf` Kernel, so an operator-triggered Kernel restart
also removes the owned worker tree rather than allowing a new Kernel epoch to
adopt it. The worker is also a direct sandboxd child with parent-death cleanup.
The exact service-manager cascade is a mandatory Linux integration test; until
that test exists, operators must treat crash/restart recovery as a P1 gate, not
as a completed HA claim.

## JVM boundary

Java/Kotlin improves in-process reliability (type safety, memory management,
and exception handling), but it is not an OS isolation boundary. A JVM OOM,
native/JNI crash, blocked native call, runaway CPU, or device-node access still
needs cgroup/process/device enforcement. JVM workers therefore run *inside* a
sandboxd-created scope just like Python workers; their language runtime never
replaces the sandbox.

## Acceptance gates

- every Rust source under `kernel/` remains free of `unsafe`, libc, direct
  process spawning, cgroup paths, pidfd, and BPF implementation;
- Kernel cannot become ready unless its configured sandbox UDS identity and
  pre-flight capabilities succeed;
- oversized, malformed, version-mismatched, or identity-mismatched UDS frames
  fail closed;
- HARD device bindings cannot silently become visibility-only;
- service restart, adapter loss, worker hang, OOM, and Docker/OCI ownership
  boundaries are covered by Linux end-to-end tests before production sign-off.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-SANDBOX-ADAPTER-BOUNDARY：进程外特权 Sandbox Host

- 状态：Accepted / Normative
- 日期：2026-08-11

## 决策

`kernel/` 是纯安全的 Rust 决策 core。它拥有 Lease、fencing、launch authorization、lifecycle state、heartbeat policy，以及本地 `cyrene.sandbox.v1` UDS 协议的通用 client。它不会启动 Worker、打开 cgroup 文件、读取 `/proc/self/cgroup`、发送 signal、调用 pidfd/BPF/prctl，也不会加载 container runtime library。

`adapters/execution/sandboxd` 是独立监管的特权 Adapter Host。首个 backend 是原生 Linux process 加 cgroup v2。它拥有委派的 cgroup 子树、只清理能证明属于自己的 `instance-*` child 的 pre-flight cleanup、限额、device-BPF 强制执行、进程回收、OOM counter，以及 SIGTERM → cgroup.kill → reap 的清理事实。若 OS API 无法避免，Rust 可以在严格审查的范围内使用 Linux `unsafe`；该例外不会跨 UDS 边界进入 Kernel 代码。

Hardware Adapter Host 仍然独立运行：它们通过 `cyrene.hardware.v1` 报告 device fact 和 binding；`sandboxd` 消费 Kernel 已批准的 binding，但绝不选择厂商，也不创建 resource Lease。若无法安装 device enforcement，要求 `HARD` 的 binding 必须失败关闭。

## System 与 Runtime Backend

host-system fact 通过 `SystemAdapter` port 暴露。当前 production build 选择 Linux system Adapter 和原生 cgroup v2 sandbox backend；面向其他目标系统的 build 必须在编译时选择一个 platform implementation。Kernel semantic authority 不会转移给 system Adapter。

Docker/OCI 是受支持的 Adapter 边界，不是 Kernel 功能，也不能通过 Core RPC 暴露原始 Docker CLI 输入。所选 profile 属于 host 配置，不是 worker 可控制的参数。每个 Worker 只有一个 backend 拥有 cgroup/lifecycle tree：

- native-cgroup：`sandboxd` 直接创建并回收 cgroup；
- oci/docker：选定 runtime 拥有 container cgroup 和 lifecycle；
- systemd scope：systemd 拥有该 scope 和 lifecycle。

禁止将 Docker 管理的 Worker 与直接 cgroup 写入混用，以免产生冲突限额和不明确的清理归属。当前实现只在 Linux 上交付和运行 `native-cgroup`。只有当 Docker/OCI Adapter 拥有 container 的完整 runtime reference、stop/reap 路径、telemetry projection 和 restart evidence 时，才可以选择它；不得把 native `cgroup_path` handle 复用为 container identifier。在 handle projection 和真实 Docker acceptance suite 完成前，Docker 只是明确的 build/runtime 扩展点，不能宣称是生产就绪的 sandbox。

## 故障与重启归属

`cyrene-sandboxd.service` 获得 cgroup delegation，且配置 `KillMode=control-group`；`cyrene-kernel.service` 不会获得 delegation。systemd 使 Kernel 排在 sandboxd 后启动，并将 sandboxd 设为 Kernel 的 `PartOf`，因此操作员重启 Kernel 时也会清理自有 Worker tree，而不是让新的 Kernel epoch 接管它。Worker 还是 sandboxd 的直接 child，并设置 parent-death cleanup。

精确的 service manager 级联行为是必需的 Linux 集成测试。在此测试实现前，运维人员必须把崩溃/重启恢复视为 P1 gate，不能将其视为已完成的 HA 保证。

## JVM 边界

Java/Kotlin 可以通过类型安全、内存管理和异常处理改善进程内可靠性，但不构成 OS 隔离边界。JVM OOM、native/JNI crash、native 调用阻塞、CPU runaway 或 device node 访问仍需要 cgroup/process/device 强制执行。因此 JVM Worker 与 Python Worker 一样运行在 sandboxd 创建的 scope 内；语言 runtime 不会替代 sandbox。

## 验收 Gate

- `kernel/` 下所有 Rust source 均不得使用 `unsafe`、libc、直接进程启动、cgroup 路径、pidfd 或 BPF 实现；
- 除非已配置的 sandbox UDS identity 和 pre-flight capability 检查成功，否则 Kernel 不得进入 Ready；
- 过大、格式错误、版本不匹配或 identity 不匹配的 UDS frame 必须失败关闭；
- HARD device binding 不得静默降级为 visibility-only；
- 生产签署前必须用 Linux 端到端测试覆盖 Service restart、Adapter loss、Worker hang、OOM 和 Docker/OCI ownership 边界。
