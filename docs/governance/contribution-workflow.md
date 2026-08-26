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
