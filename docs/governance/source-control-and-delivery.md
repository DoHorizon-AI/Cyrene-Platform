# Source Control & Delivery Responsibility Model

This document establishes the official separation of responsibilities between **GitHub** and **Azure DevOps**.

---

## 1. GitHub: The Canonical Source of Truth

**GitHub is the single source-of-truth for all source code repositories.**

```text
┌─────────────────────────────────────────────────────────────┐
│                           GITHUB                            │
│                  Source-of-Truth for Code                   │
├──────────────────────────────┬──────────────────────────────┤
│     PUBLIC REPOSITORIES      │     PRIVATE REPOSITORIES     │
│   • Cyrene-Platform          │   • Cyrene-Commercial-Plugins│
│   • Cyrene-Plugins           │   • Cyrene-Enterprise        │
│   • Public Services          │   • Internal Operations      │
└──────────────────────────────┴──────────────────────────────┘
```

- Every repository (whether Public or Private) maintains its authoritative Git history, pull requests, issue tracking, and code reviews on GitHub.
- There is **no** split where public code lives on GitHub and private code lives on Azure Repos.

---

## 2. Azure DevOps: Internal Delivery & Operations Plane

**Azure DevOps is the internal operations and enterprise delivery plane.**

```text
┌─────────────────────────────────────────────────────────────┐
│                        AZURE DEVOPS                         │
│             Internal Delivery & Operations Plane            │
├─────────────────────────────────────────────────────────────┤
│   • Multi-stage Enterprise Pipelines & Release Gates        │
│   • Private Package Feeds (Azure Artifacts / NuGet / PyPI)  │
│   • Service Connections, Cloud Subscriptions & HSM Signing  │
│   • Environment Deployment Approvals & Fleet Telemetry      │
└─────────────────────────────────────────────────────────────┘
```

- Azure DevOps checks out code from GitHub via service connections to run continuous integration, security scans, private package builds, and fleet deployments.
