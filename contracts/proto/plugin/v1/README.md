# v1 Directory Guide | contracts/proto/plugin/v1 目录指南

## Purpose | 目录职责

`plugin_protocol.proto` provides the generic worker envelope. The other ten
named SPI files are frozen `MIGRATING_COMPATIBILITY` v0 projections used only
by `cy-extension-registry`; new capabilities use generic typed payloads over
CES and must not add another named SPI file here.

`plugin_protocol.proto` 提供通用 Worker envelope。其余十个命名 SPI 文件是冻结的
`MIGRATING_COMPATIBILITY` v0 投影，只供 `cy-extension-registry` 使用；新能力通过
CES 传递通用 typed payload，不得在这里增加新的命名 SPI。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `compat_rule.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `execution_engine.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `gateway_filter.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `model_analyzer.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `notification.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `plugin_protocol.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `probe.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `quantization.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `runtime_builder.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `storage.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |
| `training_backend.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
