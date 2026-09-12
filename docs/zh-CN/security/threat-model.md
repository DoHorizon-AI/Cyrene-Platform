# Platform 信任与权限模型

英文 canonical source：[docs/security/threat-model.md](../../security/threat-model.md)。

本文是当前代码边界和部署边界的说明，不承诺资源沙箱能够隔离恶意代码。

## 组件与权威

| 组件 | 当前角色 |
| --- | --- |
| Kernel daemon | Platform Core authority；拥有 principal、lease、fence、生命周期决策和通用事件 |
| `cyrene-sandboxd` | 特权 Core 服务；拥有 cgroup、device policy、进程启动/停止和清理 |
| Node Agent | 交换 typed control message，不成为 lease authority |
| Runtime Agent | 报告 runtime state/progress，不成为第二生命周期权威 |
| Provider / Hardware Adapter | 通过版本化硬件协议报告资源事实和 binding |
| Worker / Service Plugin | 通过受控进程协议运行，产品行为留在 Platform 外部 |

进程外运行不自动改变 license 或 trust 分类；sandboxd 是这一规则的例子。

## 认证与传输

- Kernel 到 sandboxd / Hardware Adapter 使用有界、版本化 UDS frame。
- sandboxd 与 NVIDIA Adapter 可用 `SO_PEERCRED` 校验 Kernel UID/GID；Kernel 可校验
  每个配置 endpoint 的反向身份。
- Linux System Adapter 的 peer flags 当前可选；省略时依赖受保护的 socket 权限。
- Worker payload 不是 Rust trait-object Plugin ABI；外部 Worker 使用文档化 wire seam。

## 权限与非保证

生产部署必须只授予 sandboxd 和 NVIDIA sidecar 执行所需的权限，并让服务 socket 对
不可信 Worker 不可写。当前实现提供资源 accounting/capping、进程 ownership、设备
policy、可用时 pidfd 跟踪和 fencing/lifecycle integrity，但不是通用 arbitrary-code
security boundary。

当前没有 user/mount/network/PID namespace、seccomp、capability dropping、完整 host
filesystem isolation 或 syscall containment。真实 systemd crash/restart 与 cross-host
mTLS acceptance 属于部署级测试，不能从单元测试或成功编译推断出来。
