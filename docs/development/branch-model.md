# Cyrene Branching, Integration & Release Model

This document establishes the official branch topology, integration rules, and release mechanics across all Cyrene repositories.

---

## 1. Core Branch Topology

```text
feature/* (New capabilities) ────────┐
                                     ↓
fix/* (Bug fixes) ─────────────────> [ develop ]  (Normal Integration Branch - ALWAYS GREEN)
                                         │
                                   [ Release PR ]
                                         ↓
hotfix/* (Emergency fixes) ────────> [ main ]     (Release-Only Branch - Immutable History)
                                         │
                                  [ Immutable Tag ]
                                  (e.g. v0.4.3, yield/v0.2.4)
```

### A. `develop` (Normal Integration Branch)
- **Role**: Active integration branch for all public feature development and bug fixes.
- **Invariant**: **`develop` MUST ALWAYS REMAIN GREEN**. Feature branches may temporarily break, but PRs into `develop` must pass all CI gates.
- **Default Branch**: Once remote administrative migration completes, `develop` will be the default GitHub branch for cloning and PR creation.

### B. `main` (Release-Only Branch)
- **Role**: Immutable release branch representing production release history.
- **Rule**: Direct commits or ordinary feature PRs to `main` are **strictly forbidden**.
- **Promotion**: Code enters `main` exclusively via **Release PRs** from `develop` or verified **`hotfix/*` PRs**.
- **Release Automation**: Release tags and official draft/public releases are built strictly from `main` commits.

### C. `feature/*` & `fix/*`
- Branch from: `develop`
- Pull Request targets: `develop`

### D. `hotfix/*`
- Branch from: `main` (for critical release-blocking bugs)
- Pull Request targets: `main` (for tagging) $ightarrow$ then merged back into `develop`.

---

## 2. Release & Hotfix Promotion Mechanics

```text
Normal Release Flow:
  1. develop (all features merged and verified green)
  2. Open Release PR: develop -> main
  3. Merge Release PR into main
  4. Run "Prepare Component Release" workflow on main
  5. Immutable annotated tag (v<semver>) created on exact main SHA
  6. Publish Release artifacts

Hotfix Reconciliation:
  1. Branch hotfix/<name> from main
  2. Fix bug and verify
  3. PR hotfix/<name> -> main (create patch release vX.Y.Z+1)
  4. PR/Merge main -> develop (reconcile develop with the hotfix)
```
