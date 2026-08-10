# Rust Kernel runtime baseline

P2 的节点执行基线是 **纯安全 Kernel + 进程外 sandboxd + Linux cgroup v2**。
Kernel 是租约、fence token、已批准设备绑定、实例状态与心跳处置的权威；它只通过
版本化、有限帧长的 UDS 请求 sandboxd 执行启动、停止和遥测。Kernel 不创建 cgroup、
不启动 Worker、不读取 `/proc`/cgroupfs，也不包含 pidfd、BPF 或厂商 FFI。

`adapters/execution/sandboxd` 是特权执行 Host。当前 `native-cgroup` 后端负责：受委托
根的证明式清理、`cpu.max`/`memory.max`/`cpuset.cpus`、cgroup-device BPF、Worker 进程树、
`memory.current`/`cpu.stat`/`memory.events.local`、以及有界回收。硬件发现、温度和 ECC
等事实仍属于各自的 Hardware Adapter Host；sandboxd 不选择 GPU 厂商，也没有资源账本。

## 启动预检与服务顺序

`cyrene-sandboxd.service` 先启动并独占获得 cgroup delegation。它只会在自身受委托根
下清理可证明属于自己的直接 `instance-*` 子 cgroup：候选必须具有 `cgroup.procs` 与
`cgroup.kill`，杀空后才删除。它不会扫描挂载根、按全局前缀递归删除或 Adopt 不明进程。

随后 Kernel 必须以显式端点启动；以下任一失败都会保持 Not Ready：

```text
cyrene-kernel \
  --sandbox-adapter sandboxd=/run/cyrene/sandboxd.sock \
  --hardware-adapter nvidia=/run/cyrene/nvidia-adapter.sock \
  --hardware-adapter-peer-uid nvidia=992 \
  --hardware-adapter-peer-gid nvidia=992
```

- sandboxd UDS 连接、协议版本与 adapter identity 均匹配；
- sandboxd 报告 cgroup v2、`cgroup.kill`、cpu/memory/pids 控制器等必需能力；
- 各硬件 Adapter 能返回当前、身份匹配且可用的库存事实；
- 请求 HARD 设备隔离时，sandboxd 能真实加载并附加 cgroup-device BPF；失败即拒绝
  启动，绝不降级为 `CUDA_VISIBLE_DEVICES` 后仍声明 HARD。

UDS 文件权限与 peer credential 共同构成本地身份边界：服务 socket 设置为 `0660`，必须由
受信任的 Kernel/Adapter 服务账户及专用组拥有，绝不能让不受信任 Worker 可写。Kernel 可用
`--hardware-adapter-peer-uid ID=UID` / `--hardware-adapter-peer-gid ID=GID` 对已连接 Adapter
执行 Linux `SO_PEERCRED` 校验；NVIDIA Adapter 用 `--allowed-client-uid UID` /
`--allowed-client-gid GID` 对 Kernel 做反向校验。任一已配置校验失败都会在发送协议帧前拒绝。
部署必须同时配置双方或依赖受保护的 systemd socket 目录；未配置身份策略不是对不受信任
本地用户的安全边界。

## 生命周期、心跳和回收

Kernel 先验证安装记录、租约 generation/fence 和设备来源，再生成 `LaunchPlan`；
sandboxd 只执行这份已批准计划。Worker 的应用级心跳仍通过 Kernel UDS 进入，必须带有
正确 instance generation 与严格单调序号。PID 存活、D 状态、旧库存或 CPU/内存遥测都
不能证明业务健康。

超出 heartbeat deadline 后，Kernel 经 sandboxd 请求 P0 回收闭环：有界 SIGTERM、
`cgroup.kill`、wait/reap。只有进程树清空、reap 完成并返回 `complete`，Kernel 才能
发布 `STOPPED`、释放租约并再次分配设备；否则实例和设备进入 quarantine。OOM 只根据
`memory.events.local` 的 `oom_kill` 计数确认，SIGKILL 退出码本身不是 OOM 证据。

Kernel service 不再持有 Worker cgroup。sandboxd service 使用 `KillMode=control-group`
并设置为 Kernel 的 `PartOf`，因此操作员重启 Kernel 时会连带停止 sandboxd 与其 Worker
树；Worker 还是 sandboxd 的直接子进程，并带 parent-death cleanup。系统服务崩溃和自动
重启的精确级联语义必须用真实 Linux systemd 测试验证；在该 E2E 测试落地前，不能宣称
已经具备跨 Kernel 崩溃的完整自动接管能力。

## 重启、日志与围栏

`cyrene-kernel` 在接受任何 API 请求前会同步写入
`/var/lib/cyrene/runtime/journal.jsonl`（可由 `--runtime-journal` 覆盖）。这是紧凑的
JSONL 审计证据，不是 Worker 恢复数据库：只包含 node id/epoch、lease 名称、fence token、
实例名称和生命周期 reason code，绝不保存命令行、环境变量、驱动事实或 Worker 负载。

每一次 Kernel 启动都会产生单调递增的 node epoch；运行时从该节点历史 fence token 的最大值
开始分配新的 fence。于是旧 epoch 的 `ReleaseResources` 或 Worker 心跳不可能碰巧匹配一次
重启后重新使用的 lease 名称。journal 无法持久化或出现非末尾损坏记录时，Kernel 必须拒绝启动；
仅允许忽略断电留下的最后一条不完整记录。

重启流程**绝不**从 journal 恢复 `ManagedProcess`、重连或 Adopt 老 Worker。systemd 的
`PartOf=cyrene-kernel.service` 使 sandboxd 负责清理它自己能证明拥有的 Worker cgroup；新 epoch
只恢复空的资源账本并等待控制面重新 reconcile。这样 journal 不会把一次崩溃误解释为可安全
接管的运行实例。

## Docker、OCI 与 JVM Worker 的边界

Docker 不与此设计冲突，但它只能作为 sandboxd 的未来后端。每个 Worker 恰有一个
cgroup/lifecycle owner：当前是 sandboxd native-cgroup；未来要么是 OCI/Docker runtime，
要么是 systemd scope。禁止把 Docker 容器交给 sandboxd 后再由 native-cgroup 后端写其
cgroup，这会造成限制、信号和回收的双重所有权。Kernel 不接收 Docker image、命令行或
daemon 参数；后端 profile 是节点侧受审计配置。

Java/Kotlin 的 GC、类型系统和异常处理能降低 Worker 进程内部出错概率，但它们不是
操作系统隔离：JVM OOM、JNI/native crash、无限阻塞、CPU runaway 与设备节点越权都需要
外部 cgroup/进程/设备边界。因此 JVM 与 Python Worker 一样均运行在 sandboxd 创建的
作用域中；这不是对 JVM 的不信任，而是将语言可靠性和节点可靠性分层。

## 多硬件 Adapter 路由

Kernel 以重复的显式参数注册任意多个 Hardware Sidecar。`adapter_id` 只是协议身份，
不是 Kernel 的厂商枚举或动态库选择器；socket 必须是绝对 UDS 路径。每次响应身份都要
匹配配置；设备记录来源 Adapter，后续绑定只回到该来源。不同 Adapter 报告相同物理
设备 ID 时，库存刷新失败并拒绝新租约，绝不任选一方。一个 Worker 的多设备绑定仍必须
是同一 enforcement 模式；混合 HARD 和 VISIBILITY_ONLY 将失败，不会静默放宽。

每个库存响应必须声明 `sampled_at` 和 `expires_at`；Kernel 对过期、倒置或缺失 TTL 的事实
fail closed。Kernel 每隔 `--adapter-poll-interval-ms`（默认 5 秒）主动刷新并写入资源账本。
Adapter 失联或事实过期会停止新租约，向 `WatchOperations` 发布 `ADAPTER_DEGRADED`，并把已
运行实例的可观测健康状态标为 `DEGRADED`；它不会因一次断线立即杀死已有 Worker。恢复当前
事实时会发布 `KERNEL_RECONCILED`。温度、ECC 和厂商专属诊断仍仅在 Sidecar 采集，Kernel
只消费这些标准化事实与过期时间。

## 本地安装记录

Kernel 不下载、安装或解析插件安装记录。外层
`framework/crates/cy-installation-resolver` 读取安装服务已验证的
`/var/lib/cyrene/installations/<installation>/launch.json`，校验 manifest/artifact digest、
签名身份、SBOM/provenance evidence，并拒绝解析到安装目录外的可执行文件；它只经
`InstalledPluginResolver` 端口交给 Kernel 一个携带相同不可变身份的 `ResolvedLaunchPlan`。
具体 OCI、签名与安装器契约见 `docs/operations/verified-installation-record.md`。安装记录
不能覆盖 Kernel 注入的保留心跳变量，也不能越过 sandboxd 的资源与设备强制执行。
