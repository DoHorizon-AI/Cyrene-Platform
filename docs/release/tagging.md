# Cyrene Tagging, Semantic Versioning & Release Identity

This document defines Git tagging conventions, Semantic Versioning policies, and release preparation automation.

---

## 1. First Principles of Git Tags

A Git tag represents an **immutable named pointer to an exact commit SHA**.
- Normal development commits do **NOT** receive version tags.
- Published version tags must **NEVER** be moved or overwritten. If a release is defective, publish a patch version (e.g. `v0.2.1`).

---

## 2. Tag Format by Repository Classification

### A. Single-Component Repositories (`Cyrene-Platform`, `Cyrene-Yield`, `cyrene-reactor`, `cyrene-exchange`)
Format: `v<MAJOR>.<MINOR>.<PATCH>[-<PRERELEASE>]`
- Example: `v0.4.2`, `v0.2.3`, `v0.3.0-rc.1`

### B. Multi-Component Repositories (`Cyrene-Plugins`)
Because `Cyrene-Plugins` contains multiple independently versioned capabilities, tags are **component-namespaced**:
Format: `<component-id>/v<MAJOR>.<MINOR>.<PATCH>`
- Examples:
  - `hf-model-analyzer/v0.3.0`
  - `compat-rules/v0.4.1`
  - `gateway-python/v0.2.0`

### C. Future Cyrene Distribution
Format: `v<MAJOR>.<MINOR>.<PATCH>` (e.g. `v0.2.0`)

---

## 3. Automated Tag & Release Validation

Use the platform tag validator before creating release tags:
```bash
python tooling/release/validate_tag.py --repo yield --tag v0.2.3
```
