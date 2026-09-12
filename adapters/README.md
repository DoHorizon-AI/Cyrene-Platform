# adapters Directory Guide | adapters 目录指南

## Purpose | 目录职责

This directory contains Platform-owned Adapter Hosts and their protocol-facing
implementation guides.

本目录包含 Platform 自有的 Adapter Host 及其面向协议的实现指南。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `execution/` | Execution and sandbox Adapter Hosts. | 执行与沙箱适配器主机 |
| `hardware/` | System and vendor hardware Adapter Hosts. | 系统与厂商硬件适配器主机 |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read `hardware/README.md` for System/Hardware Adapter ownership, then
`execution/README.md` for the Sandbox Adapter boundary.

先读 `hardware/README.md` 了解 System/Hardware Adapter 归属，再读
`execution/README.md` 了解 Sandbox Adapter 边界。
