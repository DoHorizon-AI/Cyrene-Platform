# ADR-004: Plugins Implement Capabilities

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Cyrene must support diverse AI ecosystems (HuggingFace, vLLM, DeepSeek, FAISS, SQLite, FastMCP) without bloating core repositories.

## Decision
All modular implementations are packaged as Plugins adhering to versioned Capability contracts (e.g. `model.analyzer.v1`, `storage.provider.v1`). Plugins are discovered dynamically via official or workspace catalogs.

## Why
Decouples framework evolution from concrete third-party library dependencies. Upgrading HuggingFace transformers does not require rebuilding or redeploying Cyrene-Platform.

## Alternatives Considered
- *Static In-Tree Drivers*: Hardcode all model and storage drivers in Cyrene-Platform. Rejected due to dependency bloat and diamond version conflicts.

## Consequences
- Plugins must provide manifest metadata and conform to capability schemas validated by the conformance TCK.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-004：Plugins 实现 Capabilities

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
Cyrene 必须支持多种 AI 生态（HuggingFace、vLLM、DeepSeek、FAISS、SQLite、FastMCP），同时避免核心仓库膨胀。

## 决策
所有模块化实现都以 Plugin 形式打包，并遵循版本化 Capability 契约（例如 `model.analyzer.v1`、`storage.provider.v1`）。Plugin 通过官方目录或 workspace 目录动态发现。

## 原因
这使 framework 的演进与具体第三方库依赖解耦。升级 HuggingFace transformers 无需重新构建或部署 Cyrene-Platform。

## 考虑过的替代方案
- *树内静态驱动*：在 Cyrene-Platform 中硬编码所有模型和存储驱动。因依赖膨胀和菱形版本冲突而被拒绝。

## 后果
- Plugins 必须提供清单元数据，并符合由 conformance TCK 验证的 capability schema。
