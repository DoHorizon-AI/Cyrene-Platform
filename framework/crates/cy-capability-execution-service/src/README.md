# src Directory Guide | framework/crates/cy-capability-execution-service/src 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `binding_config.rs` | Shared, Product-neutral manifest and configured-binding assembly. | 共享且 Product-neutral 的 manifest 与 binding 组装。 |
| `lib.rs` | CES resolution, invocation, cancellation, and event-stream authority. | CES 解析、调用、取消和事件流权威。 |
| `main.rs` | Loopback-only process entrypoint with multi-binding config and readiness publication. | loopback-only、多 binding 配置及 readiness 发布的进程入口。 |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `lib.rs` | Rust implementation, contract, or test file. | Rust 实现、契约或测试文件。 |
| `main.rs` | Rust implementation, contract, or test file. | Rust 实现、契约或测试文件。 |
| `binding_config.rs` | Generic configured-binding loader and service builder. | 通用配置 binding 加载器与服务 builder。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
