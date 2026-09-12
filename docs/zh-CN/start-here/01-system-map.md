# Cyrene 系统地图

英文 canonical source：[docs/start-here/01-system-map.md](../../start-here/01-system-map.md)。

```text
Product Services
    └─ Product state, routing, scaling, business workflows
         ↓ declarative intent / execution request
Platform Framework
    └─ resolution, policy routing, installation and composition
         ↓ generic operations, leases and versioned contracts
Kernel Core
    ├─ authority, Lease/Fence, lifecycle and durable local facts
    ├─ System Adapter client ── LinuxSystemProvider (current target)
    ├─ Hardware Adapter client ── NVIDIA / other vendor sidecars
    └─ Sandbox client ── sandboxd native-cgroup-v2 (current backend)
         ↓ worker control / adapter IPC
Plugin / Worker / Provider
```

## 权威归属

| 层 | 拥有的语义 | 不拥有的语义 |
| --- | --- | --- |
| Product | 产品状态、业务流程、路由和策略 | Kernel 租约与主机生命周期 |
| Framework | discovery、resolution、组合与安装解析 | Kernel durable authority |
| Kernel | 本地准入、租约、fence、生命周期和通用事件 | GPU 厂商命令、cgroup 文件、产品状态 |
| System Adapter | 目标 OS 主机事实和通用 CPU/RAM 资源 | Lease、产品策略、GPU 厂商事实 |
| Hardware Adapter | 设备 inventory、topology、binding 和厂商 health | Kernel 资源权威 |
| Sandbox Adapter | 受批准计划的进程启动、限制和清理 | Lease issuance、产品状态 |
| Plugin / Worker | 具体能力和业务实现 | Platform Core authority |

## 运行原则

Kernel 与 Adapter 通过版本化本地 IPC 交互。进程外不等于自动成为 Plugin；
`sandboxd` 仍因拥有强制执行与清理权而属于 Platform Core。一个 Worker 的 cgroup
和生命周期树只能有一个 owner。
