# cy-workspace-client-sdk source map | 源码导航

This crate provides the lightweight client transport surface used by Workspace clients.

| File | Responsibility |
| --- | --- |
| `lib.rs` | Public client-only API and module declarations. |
| `descriptor.rs` | Validation for transport-neutral Workspace connection descriptors. |
| `transport.rs` | Outbound mTLS connection, discovery, and Workspace request clients. |

Read `descriptor.rs` before `transport.rs` when changing descriptor acceptance rules.

本 crate 提供 Workspace client 使用的轻量 transport API。

| 文件 | 职责 |
| --- | --- |
| `lib.rs` | Client-only 公共 API 与模块声明。 |
| `descriptor.rs` | transport-neutral Workspace connection descriptor 校验。 |
| `transport.rs` | 出站 mTLS 连接、发现与 Workspace request client。 |

修改 descriptor 接受规则时，先读 `descriptor.rs`，再读 `transport.rs`。
