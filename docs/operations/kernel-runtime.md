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
- Kernel 能读取 `memory.events.local`；
- pidfd 可用时优先使用，缺失时只对身份已验证的直接子进程降级到 `waitpid`；
- GPU HARD 隔离需要 cgroup device BPF；否则只能报告 `VISIBILITY_ONLY`，不能
  把 CUDA/HIP 可见性变量宣称为安全边界。

缺失硬性能力时，Kernel 保持 Not Ready。未知硬件、探测超时或缺失字段不补
默认值，也不加入可分配库存。`memory.current`、`memory.events.local` 和
`cpu.stat` 是 cgroup 资源遥测，不能替代 GPU 健康遥测；后者只能由 Adapter
Host 作为可过期的事实提供。

## Adapter 故障边界

Adapter Host 使用独立 systemd service 和 UDS 运行。连接失败、协议版本不匹配
或事实过期时，Kernel 将所依赖的设备标为 `DEGRADED`，禁止创建新的相关租约；
它不因一次 Adapter 断连立即杀死已有 Worker。已有实例的处置由租约、cgroup
遥测和业务心跳的独立证据决定。

残留清理只能针对本次 Kernel service 明确拥有的 cgroup 或带启动 epoch 的实例
目录。不得按 `cyrene` 前缀盲删整个 cgroup 树，也不得 Adopt 不明来源的进程。

## 实例回收

每个实例使用独立 cgroup。停止顺序是 Drain、协议 Shutdown、宽限期、SIGTERM、
`cgroup.kill`、有界 wait/reap。cgroup 未清空时不能发布 `STOPPED`、释放租约或
重新分配设备；实例进入 cleanup-stuck，设备进入隔离态。

OOM 由实例 cgroup 的 `memory.events.local` 中 `oom_kill` 计数差值确认。退出码
为 SIGKILL 不能单独证明 OOM。

生产服务使用 [systemd unit](../../infra/systemd/cyrene-kernel.service) 的
`KillMode=control-group` 实现 Kernel/Worker fate sharing。Kernel 重启时只清理
残留 cgroup，不 Adopt 既有进程；清理未完成前不发布库存。

每个长时间运行 Worker 还必须在约定的 IPC 窗口内报告心跳。超时先进入
`SUSPECT/DRAINING`，再按 Shutdown、SIGTERM、SIGKILL、wait/reap 的顺序处置。
单靠 PID 存活或 D 状态不能证明业务健康；该心跳闭环是 P2 生产验收门，尚未
接入时不得宣称已具备强制看门狗能力。
