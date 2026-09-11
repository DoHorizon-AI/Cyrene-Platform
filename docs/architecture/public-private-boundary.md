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
