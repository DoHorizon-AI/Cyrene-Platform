# Contribution & Review Workflow

We follow a lightweight, standard Git-flow for all Cyrene repositories.

---

## 1. Branch Naming Conventions

Create focused branches off `develop` (or `main` where develop is not configured):

| Branch Prefix | Purpose | Example |
|---|---|---|
| `feat/` | New user features or platform mechanisms | `feat/sglang-serving-engine` |
| `fix/` | Bug fixes and correctness patches | `fix/lease-timeout-fencing` |
| `docs/` | Documentation, guides, and ADRs | `docs/developer-experience-v1` |
| `refactor/` | Code structure improvements without behavior change | `refactor/catalog-schema-v1` |
| `chore/` | Tooling, dependencies, and CI updates | `chore/upgrade-pytest` |

---

## 2. Standard PR Lifecycle

```text
1. Create Branch  ──► 2. Implement & Test ──► 3. Open PR (Fill Template)
                            │
4. Merge to Target ◄── 3. Review & CI Green ◄┘
```

1. **Implement cleanly**: Adhere to boundary rules ([Where Does My Code Go?](../start-here/03-where-does-my-code-go.md)).
2. **Verify tests locally**: Run targeted tests before pushing.
3. **Fill PR template**: Note any architecture impact (contracts, kernel, persistence, public/private boundaries).
4. **CI & Review**: Require CI checks and peer review approval before merge.
---

<!-- Chinese Translation / 中文翻译 -->

# 贡献与评审流程

所有 Cyrene 仓库遵循轻量的标准 Git-flow。

---

## 1. 分支命名约定

从 `develop` 创建聚焦的分支（若仓库未配置 `develop`，则从 `main` 创建）：

| 分支前缀 | 用途 | 示例 |
|---|---|---|
| `feat/` | 新的用户功能或平台机制 | `feat/sglang-serving-engine` |
| `fix/` | 错误修复与正确性补丁 | `fix/lease-timeout-fencing` |
| `docs/` | 文档、指南与 ADR | `docs/developer-experience-v1` |
| `refactor/` | 不改变行为的代码结构改进 | `refactor/catalog-schema-v1` |
| `chore/` | 工具、依赖和 CI 更新 | `chore/upgrade-pytest` |

---

## 2. 标准 PR 生命周期

```text
1. 创建分支 ──► 2. 实现并测试 ──► 3. 创建 PR（填写模板）
                         │
4. 合并至目标 ◄── 3. Review 完成且 CI 通过 ◄┘
```

1. **规范地实现**：遵守边界规则，参见[代码应放在哪里？](../start-here/03-where-does-my-code-go.md)。
2. **在本地验证测试**：推送前运行针对性测试。
3. **填写 PR 模板**：注明对架构的影响（契约、Kernel、持久化、公开/私有边界）。
4. **CI 与评审**：合并前必须通过 CI 检查并获得同伴评审批准。
