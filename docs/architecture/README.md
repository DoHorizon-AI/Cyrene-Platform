# Architecture Guides | 架构指南

This directory explains how Kernel, Framework, Contracts, SDKs, and external
adapters cooperate without moving product semantics into the Kernel.

本目录说明 Kernel、Framework、Contracts、SDK 与进程外适配器如何协作，并确保
产品语义不会回流到 Kernel。

| File | Responsibility | 文件职责 |
| --- | --- | --- |
| `overview.md` | Component map, layers, and end-to-end data flow. | 组件图、分层与端到端数据流 |
| `system-adapter.md` | SystemAdapter port, Linux implementation, and target-build boundary. | SystemAdapter 端口、Linux 实现与目标系统构建边界 |
| `sandbox-adapter.md` | SandboxBackend port, native cgroup backend, and Docker/OCI status. | SandboxBackend 端口、native cgroup 后端与 Docker/OCI 状态 |
| `kernel-design-goals.md` | Kernel design constraints. | Kernel 设计约束 |
| `kernel-vs-platform.md` | Kernel/platform responsibility boundary. | Kernel 与 Platform 的职责边界 |
| `capability-and-plugin.md` | Capability vocabulary and plugin composition. | 能力词汇与插件组合 |
| `execution-lifecycle.md` | Lifecycle from admission to completion. | 从准入到完成的生命周期 |
| `distributed-execution-fabric-v1.md` | Frozen host, container-only, and provider-managed execution contract plus implementation map. | 冻结的主机、纯容器和 Provider-managed 执行契约及实现映射 |
| `distributed-workspace-fabric-v1.md` | Identity discovery, transport-neutral connection descriptors, and relay-first Workspace access. | 身份发现、transport-neutral 连接描述符与 relay-first Workspace 访问 |
| `artifact-and-environment.md` | Artifact and environment authority. | 产物与环境权威 |
| `product-controller-adapter.md` | Product controller and adapter placement. | 产品控制器与适配器归属 |
| `public-private-boundary.md` | Public Core versus private service boundary. | 公共 Core 与私有服务边界 |
| `tool-system.md` | Framework extension and worker system. | Framework 扩展与 Worker 系统 |
| `mcp-integration.md` | Protocol and integration boundary. | 协议与集成边界 |

## Suggested reading | 推荐顺序

Read `overview.md`, then `system-adapter.md`, `sandbox-adapter.md`,
`kernel-vs-platform.md`, `capability-and-plugin.md`, `execution-lifecycle.md`,
`distributed-execution-fabric-v1.md`, and `distributed-workspace-fabric-v1.md`
before opening implementation crates.

先读 `overview.md`，再读 `system-adapter.md`、`sandbox-adapter.md`、
`kernel-vs-platform.md`、`capability-and-plugin.md`，然后读
`execution-lifecycle.md`、`distributed-execution-fabric-v1.md` 与
`distributed-workspace-fabric-v1.md`，之后再进入实现 crate。
