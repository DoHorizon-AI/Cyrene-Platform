# v1 Directory Guide | contracts/proto/plugin/v1 目录指南

## Purpose | 目录职责

`plugin_protocol.proto` provides the generic worker envelope. Capability
payload schemas live with their owner in `Cyrene-Plugins-Official` or a Product
repository. The removed v0 named SPI tag numbers and field names are reserved;
do not add capability-specific messages or fields here.

`plugin_protocol.proto` 提供通用 Worker envelope。能力载荷 schema 归属于
`Cyrene-Plugins-Official` 或对应 Product。已删除 v0 具名 SPI 的字段号和字段名均已
保留，禁止在此重新加入能力专用 message 或字段。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `plugin_protocol.proto` | Versioned Protobuf wire contract. | 版本化 Protobuf 线协议契约。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
