# Code Ownership Guidelines

This document outlines logical component ownership across Cyrene repositories.

---

## Logical Ownership Areas

| Component / Subsystem | Primary Focus Area | Responsible Role | GitHub Team (Mapping TODO) |
|---|---|---|---|
| **Platform Kernel** (`kernel/`) | Rust node supervisor, process isolation, cgroups, leases | Kernel Maintainers | `@dohorizon/kernel-team` *(TODO)* |
| **Framework & SDK** (`framework/`, `sdk/`) | Reconciler, ExecutionPlan, Attempt, Artifacts | Platform Team | `@dohorizon/platform-team` *(TODO)* |
| **Cyrene-Yield** | Training orchestration, TrainingRun, hyperparameters | Training Leads | `@dohorizon/yield-team` *(TODO)* |
| **Cyrene-Reactor** | Serving orchestration, deployment scaling, inference | Serving Leads | `@dohorizon/reactor-team` *(TODO)* |
| **Cyrene-Plugins-Official** | Plugin manifests, official catalog, capability adapters | Ecosystem Team | `@dohorizon/plugins-team` *(TODO)* |
| **Architecture Governance** (`tooling/ci/`) | Service boundaries, dependency rules, CI guards | Architecture Guild | `@dohorizon/architecture` *(TODO)* |

> [!NOTE]
> Exact GitHub team handles will be mapped when organization-level identity provider synchronization is activated.
