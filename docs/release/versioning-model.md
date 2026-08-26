# Cyrene Semantic Versioning & Compatibility Model

This document defines how version numbers evolve across different architectural layers.

---

## 1. Version Independence Rules

1. **Component SemVer**: Each repository increments its version based on its own breaking changes, features, and fixes (`MAJOR.MINOR.PATCH`).
2. **Distribution SemVer**: The Cyrene Distribution increments its version independently based on user-facing distribution milestones and profile capabilities.
3. **Capability Interface Version**: Capability interfaces (e.g. `model.analyzer.v1`, `storage.provider.v1`) use explicit contract versioning independent of library patch versions.

---

## 2. Public vs. Private Overlays

- **Public Distribution**: Compiled purely from public component releases and public `ReleaseLock.json`.
- **Private Commercial Distribution**: Produced in Azure DevOps by applying a **Private Commercial Overlay** (e.g. proprietary accelerator plugins, Enterprise SSO) onto a base Public ReleaseLock.
- **Strict Separation**: Private overlays are never committed to public repositories.
