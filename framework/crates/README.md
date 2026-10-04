# crates Directory Guide | framework/crates 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `cy-installation-resolver/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |
| `cy-package-runtime/` | Generic package install, activation, supervision, and opaque connection facts; no capability payload routing. | 通用包安装、激活、监督与不透明连接事实；不路由能力业务载荷。 |
| `cy-platform-api/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |
| `cy-workspace-fabric/` | Legacy V1 transport and acceptance fixtures; not a production connection host. | 旧 V1 transport 与验收 fixture；不是生产连接宿主。 |
| `cy-workspace-client-sdk/` | Lightweight outbound Workspace discovery and Relay client transport. | 轻量级 Workspace 发现与 Relay 出站 client transport。 |
| `cy-workspace-control-plane/` | Workspace identity, authorization, and product-neutral authority services. | Workspace 身份、授权与产品中立 authority 服务。 |
| `cy-workspace-postgres-storage/` | PostgreSQL-backed Workspace directory and credential storage adapters. | 基于 PostgreSQL 的 Workspace directory 与凭据存储适配器。 |
| `cy-workspace-product-contracts/` | Pinned owner catalogs and trusted Platform product authorization contracts. | 固定来源 owner catalog 与可信 Platform Product 授权契约。 |
| `cy-workspace-relay-runtime/` | Legacy V1 Relay runtime retained for Fabric compatibility fixtures. | 为 Fabric 兼容 fixture 保留的旧 V1 Relay runtime。 |
| `cy-workspace-web-bff/` | Independently packaged authenticated Workspace Web BFF. | 独立打包的 Workspace Web BFF 认证服务。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
---

<!-- Chinese Translation / 中文翻译 -->

# framework/crates 目录指南

## 目录职责

本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## 内容

| 条目 | 职责 |
| --- | --- |

## 推荐阅读 / 执行顺序

先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## 内容快照

| 条目 | 职责 |
| --- | --- |
| `cy-installation-resolver/` | 嵌套源码或契约边界，下一步阅读其 README。 |
| `cy-package-runtime/` | 通用包安装、激活、监督与不透明连接事实；不路由能力业务载荷。 |
| `cy-platform-api/` | 嵌套源码或契约边界，下一步阅读其 README。 |
| `cy-workspace-fabric/` | 旧 V1 transport 与验收 fixture；不是生产连接宿主。 |
| `cy-workspace-client-sdk/` | 轻量级 Workspace 发现与 Relay 出站 client transport。 |
| `cy-workspace-control-plane/` | Workspace 身份、授权与产品中立 authority 服务。 |
| `cy-workspace-postgres-storage/` | 基于 PostgreSQL 的 Workspace directory 与凭据存储适配器。 |
| `cy-workspace-product-contracts/` | 固定来源 owner catalog 与可信 Platform Product 授权契约。 |
| `cy-workspace-relay-runtime/` | 为 Fabric 兼容 fixture 保留的旧 V1 Relay runtime。 |
| `cy-workspace-web-bff/` | 独立打包的 Workspace Web BFF 认证服务。 |

本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
