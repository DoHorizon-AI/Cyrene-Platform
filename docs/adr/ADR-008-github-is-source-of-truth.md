# ADR-008: GitHub Is Source of Truth for Source Code

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Teams evaluated whether to maintain public code on GitHub and private code exclusively on Azure Repos.

## Decision
GitHub is the single source-of-truth for all source code repositories (both Public and Private). Azure DevOps serves as the internal delivery and operations plane.

## Why
Consolidating source control on GitHub standardizes developer tooling, issue tracking, and PR reviews while leveraging Azure DevOps for secure enterprise CI/CD and deployments.

## Alternatives Considered
- *Split Source Control (GitHub + Azure Repos)*: Rejected due to git history synchronization friction and developer overhead.

## Consequences
- CI pipelines in Azure DevOps checkout repositories hosted on GitHub.
