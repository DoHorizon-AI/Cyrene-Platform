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
