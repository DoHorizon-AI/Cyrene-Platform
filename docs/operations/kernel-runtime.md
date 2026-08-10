# Rust Kernel runtime baseline

P2 的生产基线是 Linux native process + cgroup v2。Kernel 只维护本机观察
事实、租约、设备绑定和插件实例生命周期，不安装驱动、不下载插件、不执行
远程传入的 shell 或任意环境变量。

## 启动预检

节点只有在以下事实全部满足时才进入可调度状态：

- cgroup v2 已挂载且 `cpu`、`memory`、`pids` 控制器可用；
- 实例 cgroup 可创建并支持 `cgroup.kill`；
- Kernel 能读取 `memory.events.local`；
- pidfd 可用时优先使用，缺失时只对身份已验证的直接子进程降级到 `waitpid`；
- GPU HARD 隔离需要 cgroup device BPF；否则只能报告 `VISIBILITY_ONLY`，不能
  把 CUDA/HIP 可见性变量宣称为安全边界。

缺失硬性能力时，Kernel 保持 Not Ready。未知硬件、探测超时或缺失字段不补
默认值，也不加入可分配库存。

## 实例回收

每个实例使用独立 cgroup。停止顺序是 Drain、协议 Shutdown、宽限期、SIGTERM、
`cgroup.kill`、有界 wait/reap。cgroup 未清空时不能发布 `STOPPED`、释放租约或
重新分配设备；实例进入 cleanup-stuck，设备进入隔离态。

OOM 由实例 cgroup 的 `memory.events.local` 中 `oom_kill` 计数差值确认。退出码
为 SIGKILL 不能单独证明 OOM。

生产服务使用 [systemd unit](../../infra/systemd/cyrene-kernel.service) 的
`KillMode=control-group` 实现 Kernel/Worker fate sharing。Kernel 重启时只清理
残留 cgroup，不 Adopt 既有进程；清理未完成前不发布库存。
