# Cyrene Multi-Repository Release Topology

Cyrene is a modular, multi-repository AI platform composed of independently versioned components, capabilities, and products.

---

## 1. The Fundamental Invariant

$$\mathbf{1\ Git\ Repository\ \neq\ 1\ Package\ \neq\ 1\ Deployable\ \neq\ 1\ Product\ \neq\ 1\ Cyrene\ Release}$$

A single Git repository may contain multiple packages (e.g. `Cyrene-Platform` contains 4 Python SDKs, 10+ Rust crates, and JVM adapters). Conversely, a single runnable Cyrene Distribution comprises multiple repositories working together under a verified compatibility lock.

---

## 2. The Three-Level Release Model

```text
┌─────────────────────────────────────────────────────────────┐
│  LEVEL 1: SOURCE REPOSITORY (Git History & Review)          │
│  • Cyrene-Platform, Cyrene-Plugins, Cyrene-Yield, etc.     │
│  • Owns source code, PR review, issues, and unit CI.        │
├─────────────────────────────────────────────────────────────┤
│  LEVEL 2: COMPONENT ARTIFACT (Developers & Registries)      │
│  • Wheels, Crates, OCI images, JARs, Plugin bundles.        │
│  • Independently versioned (e.g. Platform 0.4.2, Yield 0.2.3)│
├─────────────────────────────────────────────────────────────┤
│  LEVEL 3: CYRENE DISTRIBUTION RELEASE (End Users & Ops)     │
│  • Verified ReleaseLock (BOM) locking exact artifact digests│
│  • Distribution Profiles (core, training, serving, full)    │
│  • User-facing release (e.g. Cyrene Distribution 0.2.0)     │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. Independent Component Versioning

We **do not** synchronize version numbers across all repositories. A release of `Cyrene Distribution 0.2.0` may legitimately consist of:
- `Cyrene-Platform`: `0.4.2`
- `Cyrene-Yield`: `0.2.3`
- `cyrene-reactor`: `0.3.7`
- `cyrene-exchange`: `0.1.9`
- `hf-analyzer` plugin: `1.2.0`
- `sqlite` plugin: `0.7.3`

---

## 4. Release is NOT Deployment

- **Public Component Release**: Building verified wheels, binaries, or container images published to GitHub Releases, GHCR, or PyPI for public consumption.
- **DoHorizon Internal Deployment**: Deploying container instances to DoHorizon-managed Azure cloud clusters via Azure DevOps delivery pipelines.

Both may originate from the exact same Git commit, but they represent completely separate lifecycle events.
