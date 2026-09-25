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
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene 公开/私有访问与责任模型

本模型规定 Cyrene 各仓库的权限、信任等级和维护职责。

---

## 1. 四种访问等级

```text
┌─────────────────────────────────────────────────────────────┐
│ TIER 1：社区贡献者                                          │
│ • 访问：公开源码、公开文档、本地验证、公开 PR CI、公开 TCK │
│ • 限制：无法访问私有仓库、Azure 或机密                     │
├─────────────────────────────────────────────────────────────┤
│ TIER 2：公开维护者                                          │
│ • 访问：Tier 1 + PR 审查、合并权限、公开发布                │
│ • 原则：公开工作绝不要求 Azure DevOps                       │
├─────────────────────────────────────────────────────────────┤
│ TIER 3：DoHorizon 内部工程师                                │
│ • 访问：Tier 2 + 经授权的私有仓库、内部集成和商业测试的 Azure Pipelines │
├─────────────────────────────────────────────────────────────┤
│ TIER 4：发布与运维维护者                                    │
│ • 访问：Tier 3 + 生产部署、代码签名、商业发布管理和机密管理 │
└─────────────────────────────────────────────────────────────┘
```
