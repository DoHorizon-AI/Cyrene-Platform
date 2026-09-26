# Cyrene Dependency & Lockfile Governance Policy

This policy governs external third-party dependencies, lockfile reproducibility, and internal cross-repository contract versioning.

---

## 1. Core Dependency Governance Rules

1. **Explicit Versioned Boundaries**: Cross-repository runtime dependencies must converge toward versioned contracts, packages, or container artifacts rather than ad-hoc relative directory traversals (e.g. `../../Cyrene-Platform/src`).
2. **Local Workspace Overrides**: Developers may use local editable paths or workspace overlays for local iteration, but CI and production builds default to reproducible versioned dependencies.
3. **Zero Private Dependencies in Public Code**: Public packages must never require private packages to install, build, or test.
4. **Isolated Plugin Environments**: **One Git repository $\neq$ one giant unified package resolution.** Different plugins (e.g. FastAPI gateway vs FAISS vector worker vs Spring Gateway) maintain their own isolated dependency scopes.

---

## 2. Ecosystem Lockfile Policies

| Ecosystem | Manifest | Canonical Lockfile | CI Verification Rule |
|---|---|---|---|
| **Python** | `pyproject.toml` | `uv.lock` | `uv sync --locked` (reproducible wheel hashes) |
| **Rust** | `Cargo.toml` | `Cargo.lock` | `cargo test --locked` (enforces exact crate graph) |
| **.NET / C#** | `*.csproj` | `Directory.Packages.props` / `packages.lock.json` | `dotnet restore --locked-mode` |
| **JVM (Gradle)** | `build.gradle.kts` | Gradle dependency verification / wrapper | `./gradlew --no-daemon check` |
| **Web / JS** | `package.json` | `pnpm-lock.yaml` / `package-lock.json` | `pnpm install --frozen-lockfile` |

---

## 3. Internal Contract & Package Versioning

We maintain strict conceptual separation between different version layers:

- **Platform Release Version**: Overall platform milestone (e.g. `0.1.0`).
- **Kernel Contract Version**: Binary & Protobuf wire protocol (e.g. `cyrene.core.v1`).
- **Capability Interface Version**: Semantic capability interface (e.g. `model.analyzer.v1`, `storage.provider.v1`).
- **Plugin Version**: Concrete implementation release version (e.g. `0.1.0` of `cyrene.models.hf-analyzer`).
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene 依赖与 Lockfile 治理政策

本政策管理外部第三方依赖、lockfile 可复现性和内部跨仓契约版本。

---

## 1. 核心依赖治理规则

1. **使用明确的版本化边界**：跨仓 runtime 依赖必须逐步采用版本化契约、package 或容器制品，不应依赖临时相对目录遍历（例如 `../../Cyrene-Platform/src`）。
2. **本地 Workspace Override**：开发者可以使用本地可编辑路径或 workspace overlay 进行迭代，但 CI 和生产构建默认使用可复现的版本化依赖。
3. **公开代码零私有依赖**：公开 package 的安装、构建和测试不得要求私有 package。
4. **隔离 Plugin 环境**：**一个 Git 仓库 ≠ 一个统一的大型 package 解析环境。**不同 plugin（例如 FastAPI gateway、FAISS vector worker、Spring Gateway）分别维护隔离的依赖作用域。

---

## 2. 生态 Lockfile 政策

| 生态 | Manifest | 规范 Lockfile | CI 验证规则 |
|---|---|---|---|
| **Python** | `pyproject.toml` | `uv.lock` | `uv sync --locked`（可复现 wheel hash） |
| **Rust** | `Cargo.toml` | `Cargo.lock` | `cargo test --locked`（强制精确 crate 依赖图） |
| **.NET / C#** | `*.csproj` | `Directory.Packages.props` / `packages.lock.json` | `dotnet restore --locked-mode` |
| **JVM（Gradle）** | `build.gradle.kts` | Gradle dependency verification / wrapper | `./gradlew --no-daemon check` |
| **Web / JS** | `package.json` | `pnpm-lock.yaml` / `package-lock.json` | `pnpm install --frozen-lockfile` |

---

## 3. 内部契约与 Package 版本

不同的版本层必须严格区分：

- **Platform Release Version**：平台整体里程碑版本（例如 `0.1.0`）。
- **Kernel Contract Version**：二进制与 Protobuf 线协议版本（例如 `cyrene.core.v1`）。
- **Capability Interface Version**：语义 capability interface 版本（例如 `model.analyzer.v1`、`storage.provider.v1`）。
- **Plugin Version**：具体实现的发布版本（例如 `cyrene.models.hf-analyzer` 的 `0.1.0`）。
