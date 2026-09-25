# hardware Directory Guide | adapters/hardware 目录指南

## Purpose | 目录职责

This directory contains the reference host-system and vendor hardware Adapter
Hosts. They report facts and bindings over versioned local protocols; they do
not become Kernel authority.

本目录包含参考的主机系统与厂商硬件 Adapter Host。它们通过版本化本地协议报告
事实与 binding，不成为 Kernel authority。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `linux_sys/` | Linux implementation of the public `SystemAdapter` port. | 公共 `SystemAdapter` 端口的 Linux 实现 |
| `nvidia/` | NVIDIA vendor inventory and binding Adapter Host. | NVIDIA 厂商 inventory 与 binding 适配器 |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Suggested reading | 推荐阅读

1. Read [`linux_sys/README.md`](linux_sys/README.md) for target-system facts.
2. Read [`nvidia/README.md`](nvidia/README.md) for vendor GPU facts.
3. Read [`docs/architecture/system-adapter.md`](../../docs/architecture/system-adapter.md)
   for the ownership and build-profile boundary.

1. 先读 [`linux_sys/README.md`](linux_sys/README.md) 了解目标系统事实。
2. 再读 [`nvidia/README.md`](nvidia/README.md) 了解厂商 GPU 事实。
3. 再读 [`docs/architecture/system-adapter.md`](../../docs/architecture/system-adapter.md)
   了解职责归属与构建 profile 边界。
---

<!-- Chinese Translation / 中文翻译 -->

# adapters/hardware 目录指南

## 目录职责

本目录包含参考性的主机系统与厂商硬件 Adapter Host。它们通过版本化本地协议报告事实和 binding，不成为 Kernel authority。

## 内容

| 条目 | 职责 |
|---|---|
| `linux_sys/` | 公共 `SystemAdapter` 端口的 Linux 实现 |
| `nvidia/` | NVIDIA 厂商 inventory 与 binding 适配器 |

## 推荐阅读与执行顺序

先读本指南，再按依赖顺序阅读上方直接文件，最后阅读嵌套目录指南。

## 推荐阅读

1. 阅读 [`linux_sys/README.md`](linux_sys/README.md)，了解目标系统事实。
2. 阅读 [`nvidia/README.md`](nvidia/README.md)，了解厂商 GPU 事实。
3. 阅读 [`docs/architecture/system-adapter.md`](../../docs/architecture/system-adapter.md)，了解职责归属和构建 profile 边界。
