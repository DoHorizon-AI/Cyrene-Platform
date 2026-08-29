# Cyrene Artifact Destinations & Distribution Portal

This policy defines where build artifacts, container images, and release assets are published.

---

## 1. Canonical Artifact Destination Matrix

| Artifact Type | Ecosystem | Primary Public Destination | Internal / Enterprise Destination |
|---|---|---|---|
| **Release Bundles & CLI** | Native Binaries | **GitHub Releases** (`DoHorizon-AI/*`) | Internal Azure Blob Storage |
| **Container Images** | OCI Images | **GHCR** (`ghcr.io/dohorizon-ai/*`) | Private Azure Container Registry (ACR) |
| **Python SDKs** | `.whl`, `.tar.gz` | **PyPI** (`cyrene-*`) | Azure Artifacts Private Feed |
| **Rust Crates** | `.crate` | **Crates.io** (when enabled) | Internal Vendor Mirror |
| **JVM Framework** | `.jar`, `.aar` | **Maven Central** (when enabled) | Azure Artifacts Maven Feed |
| **.NET Libraries** | `.nupkg` | **NuGet.org** (when enabled) | Azure Artifacts NuGet Feed |

---

## 2. Future Distribution Portal (`cyrene.ai/download`)

The future `cyrene.ai/download` portal acts as an **indexing and discovery gateway**:
- Detects user OS and accelerator hardware (NVIDIA, CPU, Apple Silicon).
- Recommends the appropriate Distribution Profile (`training`, `serving`, etc.).
- Directs downloads to verified **GitHub Releases**, **GHCR**, and public package registries.
- Does not duplicate raw storage in the initial phase.
