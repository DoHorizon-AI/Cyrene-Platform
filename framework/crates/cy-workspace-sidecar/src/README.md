# Sidecar source | Sidecar 源码

| File | Responsibility |
| --- | --- |
| `lib.rs` | Public Sidecar implementation exports. |
| `main.rs` | Loopback listener and environment configuration. |
| `sidecar.rs` | Credential checks, local bearer authentication, and remote client bridge. |

`main.rs` is the loopback-only gRPC composition root. The bridge implementation uses only the
lightweight Workspace client SDK for remote calls.

| 文件 | 职责 |
| --- | --- |
| `lib.rs` | Sidecar 实现的公共导出。 |
| `main.rs` | loopback listener 与环境配置。 |
| `sidecar.rs` | credential 校验、本地 bearer 认证与远端 client bridge。 |

`main.rs` 是仅 loopback 的 gRPC 组合入口；远程调用只使用轻量 Workspace client SDK。
