# Repository Lifecycle: Cyrene-Platform

This document provides self-contained lifecycle, boundary, and authority specifications for **Cyrene-Platform**.

---

## 1. Repository Purpose & Ownership
**Cyrene-Platform** is classified as **`PUBLIC_FOUNDATION`** with visibility **`public`**.

### What This Repository OWNS:
- Foundational contracts & Protobuf schemas
- Rust Kernel, process isolation & cgroup sandboxing
- Product Control Plane primitives
- Shared infrastructure and CI/CD tooling

### What This Repository DOES NOT OWN:
- Product-specific AI state
- Training epoch loops or Serving engine internals

---

## 2. Classification & Release Units
- **Lifecycle Class**: `PUBLIC_FOUNDATION`
- **Source Owner**: Platform Core Team
- **Independent Build Unit**: `Yes` (Build tools: cargo, pyproject, gradle)
- **Package / Artifact Units**: cyrene-control-plane, cyrene-preflight, cyrene-artifacts, cyrene-environment
- **Deployable Unit**: `Yes`
- **Product Unit**: `No (Substrate / Capability Collection)`
- **User Distribution Unit**: `No (Consumed as component in Distribution)`
- **Multi-Repository Dependency**: `No (Root trust base)`

---

## 3. Authorities & Delivery Boundaries
- **CI Authority**: `github` (Pull request validation occurs in GitHub Actions)
- **Release Role**: `COMPONENT_RELEASE`
- **Release Authority**: `github_releases`
- **Deployment Authority**: `community`
- **Distribution Profiles**: `core`, `training`, `serving`, `gateway`, `full`

---

## 4. Public / Private Trust Boundary & Access Matrix
- **Public / Private Invariant**: Strictly `PRIVATE -> PUBLIC`. Public code must never import or require private code.
- **Community Contributor**: Full access to public source, local verification, and public PR CI. Requires **zero** private tokens or Azure credentials.
- **Public Maintainer**: Review and merge PRs; releases public component artifacts without needing Azure DevOps.
- **Internal / Delivery Maintainer**: Azure DevOps pipelines are reserved for internal DoHorizon delivery and private release orchestration.

---

## 5. Participation in a Complete Cyrene Distribution
This repository does **not** distribute standalone release zip files directly to general end users. Instead, its verified component artifacts are referenced by exact commit and digest in the official **Cyrene Distribution ReleaseLock** (BOM) under the `core`, `training`, `serving`, `gateway`, `full` profile(s).

---

## 6. Verification & Governance Links
- **Local Verification**: Run `python -m pytest` or `python tooling/ci/verify.py` (Platform).
- **Canonical Architecture Docs**: See [`Cyrene-Platform/docs/start-here/00-what-is-cyrene.md`](file:///C:/Users/Baiji/DHDev/Cyrene/Cyrene-Platform/docs/start-here/00-what-is-cyrene.md)
- **Release Topology**: See [`Cyrene-Platform/docs/release/release-topology.md`](file:///C:/Users/Baiji/DHDev/Cyrene/Cyrene-Platform/docs/release/release-topology.md)
- **CI Trust Model**: See [`Cyrene-Platform/docs/governance/ci-trust-model.md`](file:///C:/Users/Baiji/DHDev/Cyrene/Cyrene-Platform/docs/governance/ci-trust-model.md)

## 7. Versioning & Tag Strategy
- **Versioning Scheme**: `semver` (SemVer)
- **Version Scope**: `repository`
- **Tag Strategy**: `repository`
- **Canonical Tag Pattern**: `v{version}`
- **Tag Immutability**: Published tags are permanent and immutable. Defective releases require patch increments.
