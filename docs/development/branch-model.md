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
- Pull Request targets: `main` (for tagging) $
ightarrow$ then merged back into `develop`.

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
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene 分支、集成与发布模型

本文规定所有 Cyrene 仓库的正式分支拓扑、集成规则和发布机制。

---

## 1. 核心分支拓扑

```text
feature/*（新能力） ───────────────┐
                                  ↓
fix/*（错误修复） ──────────────> [ develop ]（常规集成分支，始终保持绿色）
                                      │
                                 [ Release PR ]
                                      ↓
hotfix/*（紧急修复） ──────────> [ main ]（仅发布分支，历史不可变）
                                      │
                                [ Immutable Tag ]
                                （例如 v0.4.3、yield/v0.2.4）
```

### A. `develop`（常规集成分支）
- **作用**：所有公开功能开发和错误修复的活动集成分支。
- **不变量**：`develop` **必须始终保持绿色**。功能分支可能短暂不稳定，但合入 `develop` 的 PR 必须通过所有 CI gate。
- **默认分支**：远端管理迁移完成后，`develop` 将成为 GitHub 上用于 clone 和创建 PR 的默认分支。

### B. `main`（仅发布分支）
- **作用**：代表生产发布历史的不可变发布分支。
- **规则**：严格禁止直接提交或普通功能 PR 进入 `main`。
- **晋级**：代码只能通过从 `develop` 发起的 **Release PR** 或经过验证的 `hotfix/*` PR 进入 `main`。
- **发布自动化**：发布标签和正式 draft/public release 只能基于 `main` 提交构建。

### C. `feature/*` 与 `fix/*`
- 从 `develop` 创建分支。
- Pull Request 目标为 `develop`。

### D. `hotfix/*`
- 对于阻断关键发布的问题，从 `main` 创建分支。
- Pull Request 目标为 `main`（用于打标签），之后再合并回 `develop`。

---

## 2. 发布与热修复晋级机制

```text
常规发布流程：
  1. develop（所有功能均已合并并验证为绿色）
  2. 创建 Release PR：develop -> main
  3. 将 Release PR 合入 main
  4. 在 main 上运行 “Prepare Component Release” workflow
  5. 在精确 main SHA 上创建不可变 annotated tag（v<semver>）
  6. 发布制品

热修复协调：
  1. 从 main 创建 hotfix/<name> 分支
  2. 修复错误并验证
  3. 将 hotfix/<name> PR 合入 main（创建补丁版本 vX.Y.Z+1）
  4. 发起 PR 并合并 main -> develop，使 develop 纳入此热修复
```
