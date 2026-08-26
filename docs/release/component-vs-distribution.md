# Component Release vs. Cyrene Distribution Release

This document defines the distinction between developer-facing component releases and user-facing distribution releases.

---

## 1. Summary Comparison

| Dimension | Component Release | Cyrene Distribution Release |
|---|---|---|
| **Target Audience** | Software developers, package managers, deployment automation | End users, AI engineers, MLOps operators, enterprise installers |
| **Delivery Mechanism** | PyPI, Crates.io, GHCR, Maven, NuGet, Git tags | `Cyrene-Distribution` release bundle, `cyrene.ai/download` portal |
| **Contents** | Single SDK, single daemon binary, single container image, single plugin | Complete tested combination of Platform, Products, and Plugins |
| **Version Scheme** | Independent SemVer per component | Dedicated Cyrene Distribution SemVer |
| **Release Artifact** | `.whl`, `.tar.gz`, `docker push`, `.crate` | `ReleaseLock.json`, Helm chart, Compose bundle, CLI installer |

---

## 2. Release Roles per Repository

Every repository in the Cyrene ecosystem is assigned an explicit **Release Role**:

1. **`COMPONENT_RELEASE`**: Publishes versioned binary, library, or container artifacts (e.g. `Cyrene-Platform`, `Cyrene-Yield`, `cyrene-reactor`).
2. **`MULTI_COMPONENT_COLLECTION`**: Contains multiple independently versioned plugins/components rather than a single monolithic artifact (e.g. `Cyrene-Plugins`).
3. **`USER_DISTRIBUTION`**: Publishes user-facing distribution manifests, installers, and release locks (e.g. proposed future `Cyrene-Distribution`).
4. **`INTERNAL_DEPLOYMENT_ONLY`**: Deployed solely to internal infrastructure via Azure DevOps without public artifact publishing (e.g. `cyrene-dh-system-internal`).
5. **`SOURCE_ONLY`**: Shared contracts or legacy reference source without standalone binary distribution.
