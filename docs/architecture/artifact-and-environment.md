# Architecture Deep-Dive: Artifacts & Environments

Reproducibility is a non-negotiable requirement for enterprise AI workloads.

---

## 1. Artifact Immutability
- An **Artifact** represents an immutable output (e.g. model checkpoint weights, tokenizers, evaluation reports).
- Once published to an Artifact Provider, an artifact's content hash is permanently immutable.

## 2. Environment Locks & Builder Plugins
- An **Environment Lock** defines exact Python wheel hashes, CUDA runtime versions, and system package digests.
- **Why EnvironmentBuilder is a Plugin**: Different target clusters use different build backends (Docker daemon, uv virtualenv builder, Kaniko in Kubernetes). Packaging builders as Plugins allows transparent extension without modifying Platform core.
