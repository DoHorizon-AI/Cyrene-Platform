# Continuous Integration (CI) Architecture

This guide explains what CI runs on pull requests, how checks are structured, and how CI tiers operate.

---

## 1. The Four CI Execution Tiers

```text
┌─────────────────────────────────────────────────────────────┐
│  Tier 0: Static, Docs & Governance (Runs on EVERY PR)       │
│  • Markdown link validator, boundary checker (< 10s)        │
├─────────────────────────────────────────────────────────────┤
│  Tier 1: Fast Unit, Build & Conformance (Standard PR)       │
│  • Python SDK pytest, Cargo test --locked, TCK (< 2m)       │
├─────────────────────────────────────────────────────────────┤
│  Tier 2: Container & Integration (Merge / Nightly)          │
│  • Multi-worker integration, Docker image builds            │
├─────────────────────────────────────────────────────────────┤
│  Tier 3: Accelerator Hardware CI (Internal / Scheduled)     │
│  • Real NVIDIA / TPU / Ascend physical node execution       │
└─────────────────────────────────────────────────────────────┘
```

> [!IMPORTANT]
> **Tier 3 (Accelerator Hardware) is NEVER required for standard public PR merges.** Public PRs run strictly on standard CPU runners with mock hardware facts.

---

## 2. Path-Aware Workflow Optimization

To avoid wasteful compute, workflows use path filtering:
- **Docs-only PRs** (`docs/**`, `*.md`): Execute `ci / docs` and `ci / governance`, safely skipping native Rust/C++ compilation.
- **SDK changes** (`sdk/python/**`): Trigger Python test matrices and preflight tests.
- **Kernel changes** (`kernel/**`, `Cargo.*`): Trigger Rust formatting, clippy, and unit tests.
