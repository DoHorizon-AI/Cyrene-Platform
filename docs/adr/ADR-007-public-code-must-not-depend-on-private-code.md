# ADR-007: Public Code Must Not Depend on Private Code

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
To support both open-source community distribution and future enterprise/commercial offerings, a strict dependency direction is required.

## Decision
Dependency direction is strictly: $\text{PRIVATE} \longrightarrow \text{PUBLIC}$. Public code (`Cyrene-Platform`, `Cyrene-Plugins-Official`) must never import, link, or conditionally reference private commercial or enterprise packages.

## Why
Public foundation code must remain 100% buildable, testable, and functional in Community mode without requiring private repositories or licensing keys.

## Alternatives Considered
- *Conditional `try/except ImportError` Hooks in Core*: Rejected because it pollutes public source with proprietary references and obscures open-source boundaries.

## Consequences
- Private and enterprise capabilities are injected dynamically at runtime via public Capability interfaces.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-007：公开代码不得依赖私有代码

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
为同时支持开源社区发行版和未来的企业/商业产品，必须规定严格的依赖方向。

## 决策
依赖方向严格规定为 `PRIVATE → PUBLIC`。公开代码（`Cyrene-Platform`、`Cyrene-Plugins-Official`）不得导入、链接或通过条件引用私有商业或企业软件包。

## 原因
公开基础代码必须在 Community 模式下 100% 可构建、可测试且可用，不需要访问私有仓库或许可密钥。

## 考虑过的替代方案
- *Core 中的条件式 `try/except ImportError` hook*：因会将专有引用带入公开源码并模糊开源边界而被拒绝。

## 后果
- 私有和企业能力通过公开 Capability 接口在运行时动态注入。
