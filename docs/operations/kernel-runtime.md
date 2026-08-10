# Rust Kernel runtime baseline

P2 的生产基线是 Linux native process + cgroup v2。Kernel 只维护本机观察
事实、租约、设备绑定和插件实例生命周期，不安装驱动、不下载插件、不执行
远程传入的 shell 或任意环境变量。硬件厂商探测、驱动 C ABI、厂商 CLI、sysfs
拓扑和设备节点枚举均在独立 Adapter Host 中执行；Kernel 只通过版本化 UDS
Protobuf 协议读取事实和请求绑定。

## 启动预检

节点只有在以下事实全部满足时才进入可调度状态：

- cgroup v2 已挂载且 `cpu`、`memory`、`pids` 控制器可用；
- 实例 cgroup 可创建并支持 `cgroup.kill`；
- Kernel 从自身 `/proc/self/cgroup` 解析 systemd 已委托的根，并只在其 `cyrene`
  直接子树内创建实例 cgroup；绝不把 `/sys/fs/cgroup` 当作可清理根；
- Kernel 能读取 `memory.events.local`；
- pidfd 可用时优先使用，缺失时只对身份已验证的直接子进程降级到 `waitpid`；
- GPU HARD 隔离会在启动前加载并附加 cgroup-device eBPF，规则只允许 Adapter
  返回且 Kernel 再次校验过 major/minor 的字符或块设备。加载、校验或附加失败会
  拒绝本次 HARD 启动；绝不降级为 `CUDA_VISIBLE_DEVICES` 后仍声明为 HARD。

缺失硬性能力时，Kernel 保持 Not Ready。未知硬件、探测超时或缺失字段不补
默认值，也不加入可分配库存。`memory.current`、`memory.events.local` 和
`cpu.stat` 是 cgroup 资源遥测，不能替代 GPU 健康遥测；后者只能由 Adapter
Host 作为可过期的事实提供。

## 多 Adapter UDS 注册与路由

Kernel 以重复的显式参数注册一个或多个外部 Sidecar，例如：

```text
cyrene-kernel \
  --hardware-adapter nvidia=/run/cyrene/nvidia-adapter.sock \
  --hardware-adapter amd=/run/cyrene/amd-adapter.sock
```

`adapter_id` 只是稳定的协议身份，不是 Kernel 内部的厂商枚举或动态库选择器；
socket 必须是绝对 UDS 路径。Kernel 不会按厂商自动发现、加载或回退 Adapter。
每次响应的身份必须与配置匹配；设备库存记录其来源 Adapter，后续绑定请求只会
路由回该来源。不同 Adapter 报告相同物理设备 ID 时，库存刷新失败并拒绝新租约，
不会任选一方。

各 Sidecar 的 generation 仅在各自协议范围内有效。Kernel 将其当前快照组合为
自身单调递增的 inventory generation，资源账本只使用该聚合 generation 做租约
校验。当前一个进程的多设备绑定仍要求相同 enforcement 模式；混合 HARD、
VISIBILITY_ONLY 等模式会失败，而不会为了兼容性静默降级。

每个 Adapter socket 必须归受信任的 Kernel/Adapter 服务账户所有，并使用最小必要
的文件权限；当前 NVIDIA Adapter 在 bind 后显式设置 `0660`。UDS 文件权限是本地
Adapter 身份信任的第一道边界，不能把可写 socket 放给不受信任的 Worker。

## Adapter 故障边界

Adapter Host 使用独立 systemd service 和 UDS 运行。连接失败、协议版本不匹配
或事实过期时，Kernel 将所依赖的设备标为 `DEGRADED`，禁止创建新的相关租约；
它不因一次 Adapter 断连立即杀死已有 Worker。已有实例的处置由租约、cgroup
遥测和业务心跳的独立证据决定。

进程启动前，Kernel 会清理自身受委托根目录下的直接 `instance-*` cgroup：每个候选
必须同时具有 `cgroup.procs` 与 `cgroup.kill`，并且只在 `cgroup.kill` 后确认空组才
删除目录。它不会扫描挂载根、不会按全局前缀递归删除，也不会 Adopt 不明进程。

## 实例回收

每个实例使用独立 cgroup。P0 停止顺序是有界 `SIGTERM`、`cgroup.kill`、wait/reap；
cgroup 未清空时不能发布 `STOPPED`、释放租约或重新分配设备；实例进入 cleanup-stuck，
设备进入隔离态。应用级 Drain/Shutdown ACK 是后续协议扩展，当前没有伪造这一确认。

OOM 由实例 cgroup 的 `memory.events.local` 中 `oom_kill` 计数差值确认。退出码
为 SIGKILL 不能单独证明 OOM。

生产服务使用 [systemd unit](../../infra/systemd/cyrene-kernel.service) 的
`KillMode=control-group` 实现 Kernel/Worker fate sharing。Kernel 重启时只清理
残留 cgroup，不 Adopt 既有进程；清理未完成前不发布库存。

每个长时间运行 Worker 通过同一个受 `0660` 文件权限保护的 Kernel UDS 调用
`PluginLifecycleService.ReportHeartbeat`。Kernel 在启动时注入以下保留环境变量：
`CYRENE_HEARTBEAT_SOCKET`、`CYRENE_PLUGIN_INSTANCE_NAME`、
`CYRENE_PLUGIN_INSTANCE_GENERATION` 与 `CYRENE_HEARTBEAT_INTERVAL_MS`。心跳必须使用
匹配代次及严格递增序号；重复、过时代次和未知实例会得到明确 disposition。超出 deadline
后 Watchdog 执行 P0 回收序列（SIGTERM、`cgroup.kill`、wait/reap），成功后才释放租约。
单靠 PID 存活或 D 状态不能证明业务健康。

## 本地安装记录

Kernel 不下载、安装或解析插件安装记录。外层
`framework/crates/cy-installation-resolver` 读取安装服务已验证的
`/var/lib/cyrene/installations/<installation>/launch.json`，校验
`manifest_digest`、`artifact_digest`，并拒绝解析到安装目录外的可执行文件；它只
经 `InstalledPluginResolver` 端口交给 Kernel 一个 `LaunchPlan`。当前 Linux
组合二进制为 `runtime/cyrene-kernel`，因此安装布局与 JSON 解析不会进入
`kernel/` 依赖树。保留的心跳变量仍由 Kernel 注入，安装记录不能覆盖。
