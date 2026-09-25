# Cyrene Repository Architecture & Tiering Model

This document defines the canonical repository topology for Cyrene, establishing how public foundation code and private commercial/enterprise code coexist safely.

---

## 1. The Three Architectural Tiers

```text
┌────────────────────────────────────────────────────────────────────────┐
│                   PRIVATE ENTERPRISE (PROPOSED)                        │
│   Repository: Cyrene-Enterprise                                        │
│   • SSO, SAML, SCIM, Directory Sync, Multi-Tenancy                     │
│   • Enterprise RBAC, Billing, Metering, Entitlement                    │
│   • Audit Logging, Compliance, HA / Disaster Recovery                  │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ (Implements Public Interfaces)
┌───────────────────────────────────▼────────────────────────────────────┐
│              PRIVATE COMMERCIAL PLUGINS (PROPOSED)                     │
│   Repository: Cyrene-Commercial-Plugins                                │
│   • Hardware Accelerator Plugins (TPU / Ascend / Custom NPU)           │
│   • Proprietary Model Optimizations & Commercial Connectors            │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ (Implements Public Capability APIs)
┌───────────────────────────────────▼────────────────────────────────────┐
│                  PUBLIC FOUNDATION & COMMUNITY                         │
│   Repositories: Cyrene-Platform, Cyrene-Plugins-Official, Public Services │
│   • Canonical Contracts, Rust Kernel, Execution, SDKs                 │
│   • Official Community Plugins (HF, FAISS, FastMCP, SQLite)            │
│   • Public Product Services (Cyrene-Yield, Cyrene-Reactor, etc.)       │
└────────────────────────────────────────────────────────────────────────┘
```

### 1.1 Public Foundation
Public repositories define the open core trust base:
- **`Cyrene-Platform`**: Foundational contracts, Rust Kernel, execution state machines, Python SDKs, shared infrastructure, and governance tooling.
- **`Cyrene-Plugins-Official`**: Official community plugin catalog and first-party capability implementations.

### 1.2 Private Commercial Implementations (Proposed)
- **`Cyrene-Commercial-Plugins`** *(Proposed boundary)*: Houses commercial/proprietary engine implementations (e.g. specialized hardware accelerators like Google TPU or Huawei Ascend, commercial vendor connectors, and proprietary tensor kernels).

### 1.3 Private Enterprise Capabilities (Proposed)
- **`Cyrene-Enterprise`** *(Proposed boundary)*: Houses organization-scale capabilities: SSO (SAML/OIDC), SCIM directory sync, enterprise RBAC, billing/metering hooks, compliance audit sinks, and automated multi-region failover.

---

## 2. The Cardinal Dependency Rule

$$\mathbf{PRIVATE} \longrightarrow \mathbf{PUBLIC} \quad 	ext{(ALLOWED)}$$
$$\mathbf{PUBLIC} \longrightarrow \mathbf{PRIVATE} \quad 	ext{(STRICTLY FORBIDDEN)}$$

### Allowed Dependency Direction
- `Cyrene-Enterprise` may freely import and depend on `Cyrene-Platform` contracts.
- `Cyrene-Commercial-Plugins` may freely implement public `Capability` interfaces.

### Strictly Forbidden Invariants
1. `Cyrene-Platform` **must never** import or depend on `cyrene_enterprise` or `cyrene_commercial`.
2. Public service code **must never** require a private package to compile, run tests, or operate in Community mode.
3. No conditional imports of private code inside public packages (e.g. `try: import cyrene_enterprise except ImportError:` is **banned** in public repositories).
4. **Public code must remain 100% buildable, testable, and fully functional without access to private source repositories.**

---

## 3. Programming Language != Commercial Tier

Programming languages and runtime frameworks **do not** define repository visibility or commercial tiers:
- **Python $
eq$ Community**
- **ASP.NET Core $
eq$ Enterprise**
- **Spring $
eq$ Enterprise**

For example, `GatewayRuntime` publicly offers Python, ASP.NET Core, and Spring implementations in `Cyrene-Plugins-Official`. Enterprise features are discovered dynamically via capability contracts (e.g. `IdentityProvider`, `AuditSink`, `EntitlementProvider`), not by hiding entire programming languages in private repositories.
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene 仓库架构与分层模型

本文规定 Cyrene 的规范仓库拓扑，说明公开基础代码与私有商业/企业代码如何安全共存。

---

## 1. 三个架构层级

```text
┌────────────────────────────────────────────────────────────────────────┐
│                   PRIVATE ENTERPRISE（拟议）                           │
│   仓库：Cyrene-Enterprise                                               │
│   • SSO、SAML、SCIM、Directory Sync、多租户                             │
│   • Enterprise RBAC、Billing、Metering、Entitlement                     │
│   • Audit Logging、Compliance、HA / Disaster Recovery                  │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │（实现公开接口）
┌───────────────────────────────────▼────────────────────────────────────┐
│              PRIVATE COMMERCIAL PLUGINS（拟议）                        │
│   仓库：Cyrene-Commercial-Plugins                                       │
│   • 硬件加速器 Plugin（TPU / Ascend / 自定义 NPU）                     │
│   • 专有模型优化与商业 Connector                                        │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │（实现公开 Capability API）
┌───────────────────────────────────▼────────────────────────────────────┐
│                  PUBLIC FOUNDATION & COMMUNITY                         │
│   仓库：Cyrene-Platform、Cyrene-Plugins-Official、Public Services       │
│   • 规范契约、Rust Kernel、执行机制、SDK                                │
│   • 官方社区 Plugin（HF、FAISS、FastMCP、SQLite）                      │
│   • 公开 Product Service（Cyrene-Yield、Cyrene-Reactor 等）            │
└────────────────────────────────────────────────────────────────────────┘
```

### 1.1 公开基础

公开仓库定义开放核心的信任基础：
- **`Cyrene-Platform`**：基础契约、Rust Kernel、执行状态机、Python SDK、共享基础设施和治理工具。
- **`Cyrene-Plugins-Official`**：官方社区插件目录和第一方 capability 实现。

### 1.2 私有商业实现（拟议）
- **`Cyrene-Commercial-Plugins`**（拟议边界）：容纳商业/专有引擎实现，例如 Google TPU、Huawei Ascend 等专用硬件加速器、商业厂商 Connector 和专有 tensor kernel。

### 1.3 私有企业能力（拟议）
- **`Cyrene-Enterprise`**（拟议边界）：容纳组织级能力，包括 SSO（SAML/OIDC）、SCIM directory sync、enterprise RBAC、计费/metering hook、合规审计 sink 和自动多区域故障切换。

---

## 2. 首要依赖规则

$$\mathbf{PRIVATE} \longrightarrow \mathbf{PUBLIC} \quad \text{（允许）}$$
$$\mathbf{PUBLIC} \longrightarrow \mathbf{PRIVATE} \quad \text{（严格禁止）}$$

### 允许的依赖方向
- `Cyrene-Enterprise` 可以自由导入并依赖 `Cyrene-Platform` 契约。
- `Cyrene-Commercial-Plugins` 可以自由实现公开 `Capability` interface。

### 严格禁止的不变量
1. `Cyrene-Platform` **绝不能**导入或依赖 `cyrene_enterprise` 或 `cyrene_commercial`。
2. 公开 Service 代码**绝不能**要求私有 package 才能编译、运行测试或在 Community 模式下工作。
3. 公开 package 中不得条件式导入私有代码（例如 `try: import cyrene_enterprise except ImportError:` 在公开仓库中**禁止**）。
4. **公开代码必须在无法访问私有源码仓库的情况下，仍然 100% 可构建、可测试并完整运行。**

---

## 3. 编程语言不等于商业层级

编程语言和 runtime framework **不决定**仓库可见性或商业层级：
- **Python ≠ Community**
- **ASP.NET Core ≠ Enterprise**
- **Spring ≠ Enterprise**

例如，`GatewayRuntime` 可在 `Cyrene-Plugins-Official` 中公开提供 Python、ASP.NET Core 和 Spring 实现。企业功能通过 capability 契约（例如 `IdentityProvider`、`AuditSink`、`EntitlementProvider`）动态发现，而不是通过将整种编程语言藏入私有仓库来区分。
