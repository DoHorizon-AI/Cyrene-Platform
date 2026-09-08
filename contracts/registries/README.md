# registries Directory Guide | registries 目录指南

## Purpose | 目录职责

Platform does not store Product or ecosystem component instances here. Owning
Product, Plugins, and integration repositories maintain their own catalogs.
Platform only publishes generic schemas and admission contracts.
Platform 不在此存储产品或生态组件实例。产品、Plugins 与集成仓库各自维护目录；
Platform 只发布通用 Schema 与准入契约。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| No tracked registry | Product additions require no Platform change. | 产品新增不要求修改 Platform。 |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read `contracts/schemas/component-catalog.schema.json` before implementing an
external catalog.
实现外部组件目录前请阅读 `contracts/schemas/component-catalog.schema.json`。
