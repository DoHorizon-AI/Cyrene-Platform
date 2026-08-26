# Local Verification Guide

Before pushing code or opening a pull request, run the single-entry local verification tool.

---

## 1. Quick Verification (One Command)

In `Cyrene-Platform`, run:
```bash
python tooling/ci/verify.py
```

This runs all lightweight Tier 0 and Tier 1 checks:
1. Documentation link and structure validation (`tooling/docs/validate_docs.py`).
2. Architecture and service boundary governance (`tooling/ci/check_service_boundaries.py`).
3. Python SDK unit test suites (`cyrene_preflight`, `cyrene_control_plane`, `cyrene_artifacts`, `cyrene_environment`).
4. Workspace tooling diagnostics (`tooling/workspace/tests/`).

---

## 2. Scoped Verification Commands

You can also target specific subsystems:

```bash
# Validate documentation links and indexes only
python tooling/ci/verify.py --scope docs

# Run architecture boundary and governance checks only
python tooling/ci/verify.py --scope governance

# Run Python SDK tests only
python tooling/ci/verify.py --scope python

# Run Rust formatting and tests only
python tooling/ci/verify.py --scope rust
```

---

## 3. Plugin Repository Verification

In `Cyrene-Plugins`, run:
```bash
python -m pytest
```
