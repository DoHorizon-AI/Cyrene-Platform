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
│   (Cyrene-Platform, Cyrene-Plugins-Official, Public Services)        │
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
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene CI 信任模型与安全边界

本文规定公开和私有 Cyrene 仓库的持续集成所遵守的安全边界、凭证策略和信任模型。

---

## 1. 核心信任原则

$$\mathbf{PUBLIC\ CI\ MUST\ NOT\ REQUIRE\ PRIVATE\ SOURCE}$$
$$\mathbf{PUBLIC\ CI\ MUST\ NOT\ RECEIVE\ PRIVATE\ CREDENTIALS}$$
$$\mathbf{PRIVATE\ CI\ MAY\ CONSUME\ PUBLIC\ CONTRACTS\ \&\ PACKAGES}$$

```text
┌─────────────────────────────────────────────────────────────┐
│                        公开仓库                             │
│（Cyrene-Platform、Cyrene-Plugins-Official、公开 Services） │
├─────────────────────────────────────────────────────────────┤
│ • PR CI 使用标准 GitHub-hosted 公开 runner                  │
│ • GITHUB_TOKEN 默认只读                                     │
│ • 不提供私有仓库 token、云机密或 PAT                         │
│ • Fork PR 不使用凭证也必须能够完整执行                       │
└──────────────────────────────┬──────────────────────────────┘
                               ▲
                               │（消费公开 Schema 与一致性 TCK）
┌──────────────────────────────┴──────────────────────────────┐
│                        私有仓库                             │
│（Cyrene-Enterprise、Cyrene-Commercial-Plugins、内部仓库）  │
├─────────────────────────────────────────────────────────────┤
│ • 私有 CI 使用隔离的自托管 / 内部 runner                    │
│ • 根据公开 API 验证商业实现                                  │
│ • 生成私有发布制品与遥测数据                                 │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. 公开 PR 安全与 Fork 安全规则

所有公开 workflow（`pull_request`）都必须安全地处理不受信任的 fork 贡献：

1. **不检出私有源码**：公开 PR job 不得尝试克隆或引用私有仓库。
2. **不提供部署或发布机密**：PR trigger 不会获得云生产密钥、签名证书或 package 发布 token。
3. **`GITHUB_TOKEN` 最小权限**：所有 workflow 都明确声明：
   `permissions:
     contents: read`
4. **不得将机密注入 `pull_request_target`**：不得用 `pull_request_target` 运行不受信任的代码并同时提供提升权限的仓库机密。
5. **不得输出机密或泄漏遥测信息**：构建日志不得输出包含认证 token 的环境变量。

---

## 3. 可复用 Workflow 信任规则

- 公开 caller workflow 只能引用公开的可复用 workflow（例如 `DoHorizon-AI/Cyrene-Platform/.github/workflows/reusable-python-ci.yml@develop`）。
- 公开 workflow 绝不能调用私有可复用 workflow，否则 fork PR 和外部贡献者无法运行。
- 第三方 GitHub Action 必须来自受信任的一方来源，并使用版本 tag 或不可变 commit SHA。

---

## 4. 私有 Shadow CI 与商业 Gate

- **私有 Shadow CI**：私有商业仓库可以维护内部 CI job，用最新公开 `Cyrene-Platform` commit 测试私有加速器引擎（TPU、Ascend、AMD）。
- **商业发布 Gate**：商业硬件兼容性测试只约束**商业发布**，不能阻塞或使公开 Community pull request 失败。
