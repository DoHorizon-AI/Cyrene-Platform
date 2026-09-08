# cy-extension-registry Directory Guide | framework/crates/cy-extension-registry 目录指南

## Purpose | 目录职责

**Status: `MIGRATING_COMPATIBILITY`.** This implemented v0 typed proxy and
registry layer is retained until Kotlin catalog/router conformance replaces it.
It is not a supported extension point for new Platform or Product code, and it
has no production dependents inside this repository. New capabilities use the
generic resolver, CES, and WorkerControl contracts.

**状态：`MIGRATING_COMPATIBILITY`。** 该 v0 强类型代理与注册层已有具体实现，在
Kotlin catalog/router 完成一致性替代前暂时保留。新的 Platform 或 Product 代码不得
依赖或扩展它；新能力使用通用 resolver、CES 与 WorkerControl 契约。

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
| `tests/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
