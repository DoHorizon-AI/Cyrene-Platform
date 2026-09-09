# Package Guide | 包目录指南

## Purpose | 目录职责

This migration-only package exposes the frozen CES client and keeps generated
gRPC bindings behind an internal module.

本包暴露面向 Product 的 CES 客户端，并将生成的 gRPC binding 隔离在内部模块。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `__init__.py` | Stable public exports. | 稳定公共导出。 |
| `client.py` | Invocation, cancellation, deadlines, and error mapping. | 调用、取消、deadline 与错误映射。 |
| `_generated/` | Generated projection of the canonical protobuf service. | canonical protobuf 服务的生成投影。 |

## Reading order | 阅读顺序

Start with `client.py`; application code should not import `_generated`.

先读 `client.py`；应用代码不应导入 `_generated`。
