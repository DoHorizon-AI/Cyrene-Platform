# Workspace contracts | Workspace 契约

Platform owns transport-neutral Workspace discovery and API projection
contracts. The `local/v1/` namespace defines the loopback-only cross-language
sidecar surface; the `v1/` namespace defines the remote Relay and direct
Workspace Fabric protocol.

Platform 拥有与传输方式无关的 Workspace discovery 与 API projection 合同。`local/v1/`
命名空间定义仅 loopback 可访问的跨语言 sidecar 接口；`v1/` 命名空间定义远端 Relay 与
direct Workspace Fabric 协议。

| Path | Responsibility | 职责 |
| --- | --- | --- |
| `v1/workspace_fabric.proto` | Remote discovery, Relay, direct transport, and generic Workspace API. | 远程发现、Relay、direct transport 与通用 Workspace API。 |
| `local/v1/workspace_sidecar.proto` | Local authenticated gRPC boundary for non-Rust clients. | 面向非 Rust client 的本地认证 gRPC 边界。 |

Read the local sidecar contract before generating Python stubs; read the
Workspace Fabric v1 document before changing remote messages.

生成 Python stub 前先阅读本地 sidecar 合同；修改远程 message 前先阅读 Workspace Fabric v1 文档。
