# ADR-002: Products Own Product State

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
When orchestrating multi-step training or auto-scaled serving deployments, questions arose regarding where desired and observed state should reside.

## Decision
Product services (Cyrene-Yield, Cyrene-Reactor, Cyrene-Exchange) own their respective desired and observed state machines. Cyrene-Yield owns `TrainingRun` and epoch history; Cyrene-Reactor owns `Deployment` desired replicas; Cyrene-Exchange owns route topology.

## Why
Domain state models evolve rapidly with AI research. Separating product state from platform mechanisms allows services to iterate independently without requiring platform contract schema migrations.

## Alternatives Considered
- *Centralized Platform Database*: Store all product tables in Cyrene-Platform. Rejected because it violates domain boundary encapsulation.

## Consequences
- Services persist their own state and communicate with Platform via versioned SDKs and capability contracts.
---

<!-- Chinese Translation / 中文翻译 -->

# ADR-002：Products 拥有 Product 状态

- **状态**：ACCEPTED
- **日期**：2026-08-26

## 背景
在编排多步骤训练或自动扩缩容的服务部署时，需要明确期望状态和观测状态应由谁保存。

## 决策
Product 服务（Cyrene-Yield、Cyrene-Reactor、Cyrene-Exchange）分别拥有自身的期望状态机和观测状态机。Cyrene-Yield 拥有 `TrainingRun` 和 epoch 历史；Cyrene-Reactor 拥有 `Deployment` 的期望副本数；Cyrene-Exchange 拥有路由拓扑。

## 原因
AI 研究推动领域状态模型快速演进。将 Product 状态与 Platform 机制分离后，各服务可以独立迭代，无需迁移 Platform 契约 schema。

## 考虑过的替代方案
- *集中式 Platform 数据库*：在 Cyrene-Platform 存储所有 Product 表。因违反领域边界封装而被拒绝。

## 后果
- 各服务持久化自己的状态，并通过版本化 SDK 和能力契约与 Platform 通信。
