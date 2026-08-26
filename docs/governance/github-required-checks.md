# GitHub Actions Required Status Checks & Release Protections

This document specifies the exact status check names, CI gates, and release environment requirements across Cyrene repositories.

---

## 1. Required Status Checks per Repository

### A. `Cyrene-Platform` (Public Foundation)
- **`ci / python-sdks-and-tooling`**: SDK test suites, control plane contracts, preflight verification.
- **`ci / governance-and-boundaries`**: Service boundary guard, repository policy validation, documentation link verification.
- *Release Workflow (Manual Dispatch)*: **`release / prepare-draft-release`** (runs in protected environment `public-release`).

### B. `Cyrene-Plugins` (Official Public Plugins)
- **`ci / plugin-conformance`**: Plugin manifest verification, interface compliance test suites.

### C. `Cyrene-Yield` (Model Training Product)
- **`ci / unit-and-contracts`**: Training runtime unit tests, executor and checkpointing tests.
- **`ci / integration-training`**: Multi-repository integration with Platform and Plugins.

### D. `cyrene-reactor`, `cyrene-exchange`, `cyrene-astrbot-rev`, `cyrene-catalyst`, `cyrene-echo`, `cyrene-navigator`
- **`ci / verify`**: Standalone repository verification and test suite.

---

## 2. CI Authority & Public Boundary Invariants

1. **Zero Private Credentials in PR CI**: All required GitHub PR status checks run in public GitHub Actions with zero Azure DevOps credentials or tokens.
2. **Azure Pipelines are NOT PR Status Checks**: In all public repositories, `azure-pipelines.yml` has `pr: none` and `trigger: [main]`. They run exclusively as internal delivery gates.
3. **Draft Release by Default**: Automated release preparation outputs a **Draft Release**, ensuring maintainer oversight before public artifact availability.
