# execution Directory Guide | adapters/execution 目录指南

## Purpose | 目录职责

This directory contains execution Adapter Hosts that implement the Core
`SandboxBackend` boundary. The current shipped backend is native Linux cgroup
v2; Docker/OCI remains a documented future boundary.

本目录包含实现 Core `SandboxBackend` 边界的执行 Adapter Host。当前交付后端是
Linux native cgroup v2；Docker/OCI 仍是已记录的未来边界。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `sandboxd/` | Privileged native cgroup v2 Sandbox Adapter Host. | 特权 native cgroup v2 沙箱适配器主机 |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Suggested reading | 推荐阅读

Read [`sandboxd/README.md`](sandboxd/README.md), then
[`docs/architecture/sandbox-adapter.md`](../../docs/architecture/sandbox-adapter.md)
for the Core ownership and Docker/OCI status.

先读 [`sandboxd/README.md`](sandboxd/README.md)，再读
[`docs/architecture/sandbox-adapter.md`](../../docs/architecture/sandbox-adapter.md)，
了解 Core 职责归属与 Docker/OCI 状态。
