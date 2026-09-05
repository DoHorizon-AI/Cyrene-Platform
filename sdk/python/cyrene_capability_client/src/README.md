# Source Guide | 源码目录指南

## Purpose | 目录职责

This directory contains the installable Python sources for the canonical
Capability Execution Service client.

本目录包含 canonical Capability Execution Service 客户端的可安装 Python 源码。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `cyrene_capability_client/` | Public client facade and generated protocol projection. | 公共客户端门面与生成的协议投影。 |

## Reading order | 阅读顺序

Read `cyrene_capability_client/client.py`, then the generated projection only
when debugging wire compatibility.

先阅读 `cyrene_capability_client/client.py`，仅在排查 wire compatibility 时阅读生成代码。
