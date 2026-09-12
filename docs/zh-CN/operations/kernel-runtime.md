# Kernel Runtime 运维基线

英文 canonical source：[docs/operations/kernel-runtime.md](../../operations/kernel-runtime.md)。

Linux P2 执行基线是 **纯安全 Kernel + 进程外 sandboxd + Linux cgroup v2**。
Kernel 是租约、fence token、已批准 binding、实例状态和 heartbeat 处置的权威；它不
创建 cgroup、不启动 Worker、不读取 `/proc`/cgroupfs，也不包含 pidfd、BPF 或厂商 FFI。

## 服务关系

- `cyrene-sandboxd` 先启动并获得 cgroup delegation；它只清理能证明属于自己的
  `instance-*` 子 cgroup。
- `cyrene-linux-sys-adapter` 提供 Linux host inventory，是 Linux 构建的必需 host-facts
  provider。
- `cyrene-nvidia-adapter` 提供 NVIDIA inventory 和 binding；GPU topology 与厂商事实
  不由 System Adapter 负责。
- Kernel 通过显式绝对 UDS endpoint 注册每个 Adapter，并校验响应 identity、inventory
  generation 和 TTL。

## 身份与 fail-closed

UDS 文件权限与 peer credential 共同构成本地身份边界。生产部署应在 Kernel 与每个
Adapter 两侧显式配置 UID/GID。sandboxd 和 NVIDIA Adapter 当前要求可信 peer；Linux
System Adapter 仍允许省略 peer flags，因此不能把默认 socket 权限模型误解为强制进程
身份认证。

## 生命周期与恢复

Kernel 先验证安装记录、lease generation/fence 和设备来源，再生成 `LaunchPlan`；
sandboxd 只执行批准计划。heartbeat 超时进入有界 SIGTERM → `cgroup.kill` → wait/reap
闭环；只有清理确认后才能发布 `STOPPED` 并释放资源。OOM 以
`memory.events.local` 的 `oom_kill` 计数确认，不能仅凭 SIGKILL 推断。

systemd 的 `PartOf=cyrene-kernel.service` 让 Kernel 的操作员重启同时停止 sandboxd
及其 Worker 树。跨崩溃自动恢复的精确级联语义仍需要真实 Linux systemd E2E 验证，
不能仅凭单元测试或编译结果宣称完成。

## Docker/OCI 边界

当前 Worker 的 cgroup/lifecycle owner 是 sandboxd native-cgroup。未来可以由 OCI/Docker
runtime 或 systemd scope 完整拥有，但禁止把 Docker 容器交给 sandboxd 后再让 native
cgroup 后端写同一棵树，否则会产生双重限制与清理权威。
