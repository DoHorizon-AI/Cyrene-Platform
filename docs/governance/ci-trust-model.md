# Cyrene CI Trust Model & Security Boundary

This document defines the security boundaries, credential policies, and trust models governing Continuous Integration across public and private Cyrene repositories.

---

## 1. The Core Trust Principle

$$\mathbf{PUBLIC\ CI\ MUST\ NOT\ REQUIRE\ PRIVATE\ SOURCE}$$
$$\mathbf{PUBLIC\ CI\ MUST\ NOT\ RECEIVE\ PRIVATE\ CREDENTIALS}$$
$$\mathbf{PRIVATE\ CI\ MAY\ CONSUME\ PUBLIC\ CONTRACTS\ \&\ PACKAGES}$$

```text
┌─────────────────────────────────────────────────────────────┐
│                    PUBLIC REPOSITORIES                      │
│   (Cyrene-Platform, Cyrene-Plugins, Public Services)        │
├─────────────────────────────────────────────────────────────┤
│ • PR CI runs on standard GitHub-hosted public runners       │
│ • GITHUB_TOKEN has default read-only permissions            │
│ • Zero private repo tokens, zero cloud secrets, zero PATs   │
│ • Fork PRs execute 100% cleanly without credentials         │
└──────────────────────────────┬──────────────────────────────┘
                               ▲
                               │ (Consumes Public Schemas & Conformance TCK)
┌──────────────────────────────┴──────────────────────────────┐
│                    PRIVATE REPOSITORIES                      │
│   (Cyrene-Enterprise, Cyrene-Commercial-Plugins, Internal)  │
├─────────────────────────────────────────────────────────────┤
│ • Private CI runs on isolated self-hosted / internal runners│
│ • Validates commercial implementations against public APIs  │
│ • Produces private release artifacts & telemetry            │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. Public PR Security & Fork Safety Rules

All public workflows (`pull_request`) must be strictly safe for untrusted fork contributions:

1. **No Private Source Checkouts**: Public PR jobs must never attempt to clone or reference private repositories.
2. **Zero Deployment / Publishing Secrets**: PR triggers never receive cloud production keys, signing certificates, or package publishing tokens.
3. **Least-Privilege `GITHUB_TOKEN`**: All workflows explicitly declare:
   ```yaml
   permissions:
     contents: read
   ```
4. **No `pull_request_target` with Secret Injection**: We do not use `pull_request_target` to run untrusted code with elevated repository secrets.
5. **No Secret Printing / Telemetry Leakage**: Build logs must never emit environment variables containing auth tokens.

---

## 3. Reusable Workflow Trust Rules

- Public caller workflows may **only** reference **Public reusable workflows** (e.g. `DoHorizon-AI/Cyrene-Platform/.github/workflows/reusable-python-ci.yml@develop`).
- Public workflows must **never** call private reusable workflows (which would fail for fork PRs and external contributors).
- Third-party GitHub Actions must use trusted first-party sources with version tags or immutable commit SHAs.

---

## 4. Private Shadow CI & Commercial Gating

- **Private Shadow CI**: Private commercial repositories may maintain internal CI jobs that test private accelerator engines (TPU, Ascend, AMD) against the latest public `Cyrene-Platform` commit.
- **Commercial Release Gate**: Commercial hardware compatibility tests gate **Commercial Releases**, never blocking or failing public Community pull requests.
