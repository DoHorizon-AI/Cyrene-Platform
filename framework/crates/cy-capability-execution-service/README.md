# cy-capability-execution-service Directory Guide | framework/crates/cy-capability-execution-service 目录指南

## Purpose | 目录职责

Status: **`MIGRATING_COMPATIBILITY`**.

This crate is the frozen CES proxy for measured Astrbot and Exchange
consumers. New Products resolve and authorize a generic Platform endpoint and
then invoke the Plugin directly with its owner contract.

本 crate 是仅供现有 Astrbot 与 Exchange 迁移的冻结 CES 代理。新 Product 通过
Platform 获取通用 endpoint 与授权，然后使用 Plugin owner 契约直接调用插件。

`InvokeCapability` remains the compatibility unary path. The additive
`InvokeCapabilityStream` method is used only when the worker handshake advertises
`cyrene.worker.typed-invocation-stream.v1`; it sends an independent
`Invoke.stream_results` flag and forwards ordered typed chunks with one terminal
outcome. A chat payload's `stream` field never selects the worker transport.

`InvokeCapability` 继续作为兼容 unary 路径。新增的 `InvokeCapabilityStream` 只有在
worker 握手声明 `cyrene.worker.typed-invocation-stream.v1` 后才启用；它设置独立的
`Invoke.stream_results` 标志，并透传有序类型化 chunk 与唯一 terminal outcome。chat
payload 的 `stream` 字段不会选择 worker 传输方式。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `src/lib.rs` | Frozen CES compatibility implementation and gRPC projection. | 冻结 CES 兼容实现及 gRPC 投影。 |
| `src/main.rs` | Production process entrypoint for one manifest and one or more configured bindings. | 单 manifest、多配置 binding 的正式进程入口。 |
| `src/binding_config.rs` | Shared manifest/binding configuration loader. | 共享 manifest/binding 配置加载器。 |
| `tests/service_tck.rs` | Repository-local CES lifecycle TCK. | 仓内 CES 生命周期 TCK。 |

## Process configuration | 进程配置

The binary requires `--manifest PATH`. `--bindings PATH` accepts a non-empty
JSON array of `{ "id": string, "environment": { string: string } }` records;
IDs must be non-empty and unique. `--ready-file PATH` publishes the resolved
ephemeral endpoint and is removed on graceful exit. TCP is intentionally
restricted to loopback because this process does not provide transport
authentication; a remote deployment must inject an authenticated transport.

二进制要求提供 `--manifest PATH`。`--bindings PATH` 接受非空 JSON 数组，每项为
`{ "id": string, "environment": { string: string } }`；ID 必须非空且唯一。
`--ready-file PATH` 发布实际 endpoint，并在优雅退出时移除。该进程尚无传输认证，
因此 TCP 只允许 loopback；远程部署必须注入已认证传输。

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `Cargo.toml` | Rust package manifest. | Rust 包清单。 |
| `src/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |
| `tests/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
