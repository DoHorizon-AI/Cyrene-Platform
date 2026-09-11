# Linux System Adapter | Linux 系统适配器

`cyrene-linux-sys-adapter` is the Linux implementation of the public
`SystemAdapter` port. It reports host-system facts and generic CPU/RAM resource
projections; it is not the Kernel authority and it is not the NVIDIA vendor
adapter.

`cyrene-linux-sys-adapter` 是公共 `SystemAdapter` 端口的 Linux 实现。它报告
主机系统事实，并提供通用 CPU/RAM 资源投影；它不是 Kernel 权威，也不是 NVIDIA
厂商适配器。

## Responsibilities | 职责

- Reads Linux host facts through the provider implementation (`/proc`, kernel
  release, and NUMA-related host information).
- Exposes `cpu-host` and `ram-host` resources through the versioned
  `cyrene.hardware.v1` framed UDS protocol.
- Creates soft CPU/RAM bindings with no device-node injection; Kernel cgroup
  policy remains the allocation/enforcement mechanism.
- Reports inventory generation, readiness facts, and resource health.
- Admits only configured local peers when UID/GID checks are supplied.

- 通过 provider 实现读取 Linux 主机事实（`/proc`、内核版本和 NUMA 相关信息）。
- 通过版本化的 `cyrene.hardware.v1` 有界 UDS 帧协议暴露 `cpu-host` 与 `ram-host`。
- 创建不注入设备节点的 Soft CPU/RAM binding；Kernel cgroup 策略仍是分配与执行机制。
- 报告 inventory generation、就绪事实和资源健康状态。
- 配置 UID/GID 校验时，只接受被允许的本地 peer。

## Files | 文件

| Path | Responsibility | 职责 |
| --- | --- | --- |
| `src/probe.rs` | Linux fact parsing, inventory, binding, and health. | Linux 事实解析、库存、绑定与健康状态 |
| `src/lib.rs` | Versioned request/response mapping. | 版本化请求/响应映射 |
| `src/main.rs` | UDS adapter host process and peer admission. | UDS 适配器进程与 peer 准入 |
| `src/peer.rs` | Unix peer-credential checks. | Unix peer credential 校验 |
| `src/tests.rs` | Provider and protocol tests. | provider 与协议测试 |

## Build and runtime boundary | 构建与运行边界

The public port is target-neutral, but this implementation is Linux-only. A
non-Linux build exits as unsupported; another target must provide a separate
`SystemAdapter` implementation and be selected for that target build. Only the
selected system adapter is active for a node.

公共端口与目标系统无关，但本实现仅支持 Linux。非 Linux 构建会以不支持状态退出；
其他目标系统必须提供独立的 `SystemAdapter` 实现，并在对应目标构建中选择它。一个
节点只启用被选择的 system adapter。

The shipped systemd unit is
`infrastructure/systemd/cyrene-linux-sys-adapter.service`. Its endpoint and
service identity must match the Kernel registration described in
[`docs/operations/kernel-runtime.md`](../../../docs/operations/kernel-runtime.md).

随仓库提供的 systemd unit 是
`infrastructure/systemd/cyrene-linux-sys-adapter.service`。它的 endpoint 和服务身份
必须与 [`docs/operations/kernel-runtime.md`](../../../docs/operations/kernel-runtime.md)
中 Kernel 的注册配置一致。

## Status | 当前状态

| Area | Status | Evidence / boundary |
| --- | --- | --- |
| Linux inventory | `COMPLETE` | `LinuxSystemProvider` implements `HostInventoryProvider`. |
| CPU/RAM binding | `COMPLETE` | `ResourceProvider::create_binding` returns soft host-resource bindings. |
| Public port | `COMPLETE` | `SystemAdapter` is defined in `cy-kernel-contract`. |
| Non-Linux implementation | `DEFERRED` | No Windows/macOS/other target provider is shipped. |
| Production deployment acceptance | `DEFERRED` | Real systemd, root, and second-user validation remains environment-required. |

| 范围 | 状态 | 证据 / 边界 |
| --- | --- | --- |
| Linux inventory | `COMPLETE` | `LinuxSystemProvider` 实现 `HostInventoryProvider`。 |
| CPU/RAM binding | `COMPLETE` | `ResourceProvider::create_binding` 返回主机资源 Soft binding。 |
| 公共端口 | `COMPLETE` | `SystemAdapter` 定义在 `cy-kernel-contract`。 |
| 非 Linux 实现 | `DEFERRED` | 当前未提供 Windows/macOS/其他目标系统 provider。 |
| 生产部署验收 | `DEFERRED` | 真实 systemd、root 与 second-user 验证仍需要相应环境。 |

For the authoritative ownership and protocol description, read
[`docs/architecture/system-adapter.md`](../../../docs/architecture/system-adapter.md).

权威的职责与协议说明请阅读
[`docs/architecture/system-adapter.md`](../../../docs/architecture/system-adapter.md)。
