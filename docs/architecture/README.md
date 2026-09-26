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
| `no-port-connectivity-plan.md` | Plan: account and device-code identity, LAN_DIRECT in-cluster, relay fallback, direct Artifact transfer, and the logging close-out. Status: PROPOSED. | 计划：账号与设备码身份、集群内 LAN_DIRECT、relay 回退、Artifact 直连传输与日志收口。状态：提案中 |
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
---

<!-- Chinese Translation / 中文翻译 -->

# 架构指南

本目录说明 Kernel、Framework、Contracts、SDK 与进程外 adapter 如何协作，同时避免将 Product 语义移入 Kernel。

| 文件 | 职责 |
|---|---|
| `overview.md` | 组件图、分层和端到端数据流。 |
| `system-adapter.md` | SystemAdapter port、Linux 实现和目标构建边界。 |
| `sandbox-adapter.md` | SandboxBackend port、原生 cgroup 后端和 Docker/OCI 状态。 |
| `kernel-design-goals.md` | Kernel 设计约束。 |
| `kernel-vs-platform.md` | Kernel 与 Platform 的职责边界。 |
| `capability-and-plugin.md` | Capability 词汇和 Plugin 组合。 |
| `execution-lifecycle.md` | 从准入到完成的生命周期。 |
| `distributed-execution-fabric-v1.md` | 冻结的 host、纯容器和 Provider-managed 执行契约及实现映射。 |
| `distributed-workspace-fabric-v1.md` | 身份发现、transport-neutral 连接描述符和 relay-first Workspace 访问。 |
| `no-port-connectivity-plan.md` | 计划：账号与设备码身份、集群内 LAN_DIRECT、relay 回退、Artifact 直连传输和日志收口。状态：提案中。 |
| `artifact-and-environment.md` | Artifact 与环境 authority。 |
| `product-controller-adapter.md` | Product controller 与 adapter 的放置。 |
| `public-private-boundary.md` | Public Core 与 private Service 的边界。 |
| `tool-system.md` | Framework 扩展与 Worker 系统。 |
| `mcp-integration.md` | 协议与集成边界。 |

## 推荐顺序

先读 `overview.md`，再读 `system-adapter.md`、`sandbox-adapter.md`、`kernel-vs-platform.md`、`capability-and-plugin.md`、`execution-lifecycle.md`、`distributed-execution-fabric-v1.md` 和 `distributed-workspace-fabric-v1.md`，然后再阅读实现 crate。
