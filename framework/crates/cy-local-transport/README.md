# cy-local-transport Directory Guide | framework/crates/cy-local-transport 目录指南

## Purpose | 目录职责

**Status: `MIGRATING_COMPATIBILITY`.** This implemented v0 stdio transport is
retained for compatibility tests only. It has no production dependents in this
repository. Production workers use Kernel-managed process and sandbox
boundaries; new code must not depend on this crate.

**状态：`MIGRATING_COMPATIBILITY`。** 该 v0 stdio transport 只为兼容测试保留，
仓内没有生产依赖。生产 Worker 使用 Kernel 管理的进程和沙箱边界，新代码不得依赖本 crate。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `Cargo.toml` | Rust package manifest. | Rust 包清单。 |
| `src/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
