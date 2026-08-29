# Cyrene Public/Private Access & Responsibility Model

This model defines permissions, trust tiers, and maintenance responsibilities across repositories.

---

## 1. The Four Access Tiers

```text
┌─────────────────────────────────────────────────────────────┐
│  TIER 1: COMMUNITY CONTRIBUTOR                              │
│  • Access: Public source, public docs, local verification,  │
│            public PR CI (GitHub Actions), public TCK.       │
│  • Restrictions: Zero access to private repos, Azure, secrets│
├─────────────────────────────────────────────────────────────┤
│  TIER 2: PUBLIC MAINTAINER                                  │
│  • Access: Tier 1 + PR review, merge rights, public releases │
│  • Principle: NEVER requires Azure DevOps for public work.  │
├─────────────────────────────────────────────────────────────┤
│  TIER 3: INTERNAL DO HORIZON ENGINEER                       │
│  • Access: Tier 2 + Authorized private repos, Azure Pipelines│
│            for internal integration and commercial tests.   │
├─────────────────────────────────────────────────────────────┤
│  TIER 4: RELEASE & OPERATIONS MAINTAINER                    │
│  • Access: Tier 3 + Production deployment, code signing,    │
│            commercial release management, secret management. │
└─────────────────────────────────────────────────────────────┘
```
