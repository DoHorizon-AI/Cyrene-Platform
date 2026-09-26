# Architecture Deep-Dive: Public & Private Boundary

Cyrene is architected so that the open-source community edition and future proprietary/enterprise editions share a single, unified foundation.

---

## 1. Core Architectural Principle

> **Public code defines contracts. Private code implements optional capabilities.**

```text
┌─────────────────────────────────────────────────────────────┐
│                      PUBLIC FOUNDATION                      │
│   • Defines: Capability Interfaces, Schemas, Kernel Core    │
│   • Implements: Community Providers (HF, FAISS, SQLite)     │
└──────────────────────────────┬──────────────────────────────┘
                               ▲
                               │ (Implements Public Contracts)
┌──────────────────────────────┴──────────────────────────────┐
│                 PRIVATE / COMMERCIAL TIERS                  │
│   • Implements: TPU/Ascend Engines, Enterprise SSO, RBAC    │
└─────────────────────────────────────────────────────────────┘
```

1. **Zero Private Dependencies in Public**: Public repositories (`Cyrene-Platform`, `Cyrene-Plugins-Official`) compile, test, and run out-of-the-box with zero references to private packages.
2. **Dynamic Capability Handshake**: When an enterprise or commercial plugin is present in the environment, the Platform's Capability Resolver discovers it and binds it seamlessly.
---

<!-- Chinese Translation / 中文翻译 -->

# 架构深入解析：公开与私有边界

Cyrene 的架构使开源社区版与未来的专有/企业版共享统一基础。

---

## 1. 核心架构原则

> **公开代码定义契约。私有代码实现可选能力。**

```text
┌─────────────────────────────────────────────────────────────┐
│                      PUBLIC FOUNDATION                      │
│   • 定义：Capability Interface、Schema、Kernel Core          │
│   • 实现：社区 Provider（HF、FAISS、SQLite）                 │
└──────────────────────────────┬──────────────────────────────┘
                               ▲
                               │（实现公开契约）
┌──────────────────────────────┴──────────────────────────────┐
│                 PRIVATE / COMMERCIAL TIERS                  │
│   • 实现：TPU/Ascend Engine、Enterprise SSO、RBAC            │
└─────────────────────────────────────────────────────────────┘
```

1. **公开代码不依赖私有代码**：公开仓库（`Cyrene-Platform`、`Cyrene-Plugins-Official`）可以开箱即用地编译、测试和运行，且不包含任何对私有 package 的引用。
2. **动态 Capability 握手**：企业或商业 Plugin 出现在环境中时，Platform 的 Capability Resolver 会发现它并无缝建立绑定。
