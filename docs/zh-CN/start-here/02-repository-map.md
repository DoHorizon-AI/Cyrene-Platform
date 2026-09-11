# 仓库地图

英文 canonical source：[docs/start-here/02-repository-map.md](../../start-here/02-repository-map.md)。

| 目录 | 职责 |
| --- | --- |
| `contracts/` | Protobuf、schema、语义模型、生成绑定和 TCK |
| `kernel/` | Kernel authority、Lease/Fence、资源管理和 adapter client |
| `framework/` | Product-neutral execution、解析与安装边界 |
| `runtime/` | Kernel composition root 和 host runtime |
| `agents/` | Node Agent 与 Runtime Agent |
| `adapters/hardware/linux_sys/` | Linux System Adapter |
| `adapters/hardware/nvidia/` | NVIDIA Hardware Adapter |
| `adapters/execution/sandboxd/` | Privileged native cgroup Sandbox Adapter Host |
| `sdk/` | 对外 SDK |
| `infrastructure/` | systemd units 与部署参考 |
| `tooling/` | 治理、同步、外部 consumer 和验收门禁 |
| `docs/` | 架构、契约、安全、运维和发布文档 |

“Product 仓库不能因为增加一个产品方法而修改 Platform”是重要边界。跨仓库能力
应通过版本化 contract、SDK 或公开进程 seam 连接。
