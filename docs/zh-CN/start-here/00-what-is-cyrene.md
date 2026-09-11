# Cyrene 是什么

英文 canonical source：[docs/start-here/00-what-is-cyrene.md](../../start-here/00-what-is-cyrene.md)。

CYRENE Platform 是可复用的 Rust 系统基座与 Kernel。它提供节点本地资源事实、
资源租约与 fencing、进程生命周期、运行证据、版本化协议和可替换 Adapter 边界。
它不是 AI 产品框架，也不拥有模型、训练、对话、计费或产品路由语义。

## 五个关键边界

1. **Platform 提供机制。** 资源、租约、生命周期、协议、安装记录和运行时边界属于
   可复用基础设施。
2. **Kernel 保持通用。** Kernel 不理解 Transformer、CUDA kernel、token、epoch 或
   产品工作流。
3. **Product 拥有产品状态。** Yield、Reactor、Exchange 等产品仓库拥有自己的
   TrainingRun、Deployment、路由和业务状态。
4. **Plugin 提供可替换能力。** Plugin 通过版本化 capability contract 运行，不把
   实现链接进 Platform Core。
5. **Adapter 隔离系统差异。** System Adapter 提供目标系统事实，Hardware Adapter
   提供厂商设备事实，Sandbox Adapter 执行受批准的进程计划。

## 不能混淆的概念

- `SystemAdapter` 不是 Kernel authority；它只提供一个目标系统的事实和资源投影。
- NVIDIA Adapter 不是 System Adapter；GPU topology、设备节点和厂商健康属于硬件
  适配器。
- `SandboxBackend` 不是公开第三方 Plugin ABI；当前 native cgroup 后端仍是 Core
  的强制执行服务。
- Docker/OCI 是未来 Sandbox Adapter 边界，不代表当前已交付 Docker sandbox。

## 当前发布状态

仓库处于 pre-release。Linux System Adapter 和 native cgroup 生命周期实现已存在，
但非 Linux provider、Docker 后端、真实特权 systemd 部署验收和 hostile-code
containment 仍不是当前完成项。
