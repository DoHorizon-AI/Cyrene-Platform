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
