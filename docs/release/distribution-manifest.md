# Cyrene Distribution Manifest & ReleaseLock Model

A Cyrene Distribution is defined declaratively through a **Release Specification** compiled into an immutable **ReleaseLock** (Bill of Materials).

---

## 1. Proposed Future `Cyrene-Distribution` Repository

The proposed future `Cyrene-Distribution` repository will serve as the **Distribution Metadata Plane**.

> [!IMPORTANT]
> `Cyrene-Distribution` MUST NOT become a monolithic parent Git repo, a submodule container, or a mirror of all source code. It only contains release manifests, profiles, installers, Compose/Helm templates, and release notes.

---

## 2. The Release Compilation Flow

$$\mathbf{ReleaseSpec} \xrightarrow{\text{Compatibility Validation}} \mathbf{ReleaseLock\ (Release\ BOM)}$$

```text
┌─────────────────────────────────────────────────────────────┐
│                      ReleaseSpec.yaml                       │
│ • Declares semantic version targets and requested profiles  │
│   e.g. platform: ^0.4.0, yield: ^0.2.0, plugins: [hf, lora] │
└──────────────────────────────┬──────────────────────────────┘
                               │ (TCK & Compatibility Validation)
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                      ReleaseLock.json                       │
│ • Locks EXACT immutable Git commit SHAs and artifact digests│
│ • Guarantees 100% reproducible deployment across nodes       │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. Conceptual ReleaseLock Schema

```json
{
  "schemaVersion": "cyrene.distribution.v1",
  "distributionVersion": "0.2.0",
  "releasedAt": "2026-08-26T19:00:00Z",
  "profiles": ["core", "training", "serving", "gateway", "full"],
  "components": {
    "platform": {
      "version": "0.4.2",
      "gitCommit": "fce6093...",
      "artifacts": {
        "kernelDaemon": "ghcr.io/dohorizon-ai/cyrene-kernel:0.4.2@sha256:...",
        "pythonSdkPreflight": "cyrene_preflight==0.4.2"
      }
    },
    "yield": {
      "version": "0.2.3",
      "gitCommit": "b0ded05...",
      "artifacts": {
        "controller": "ghcr.io/dohorizon-ai/cyrene-yield:0.2.3@sha256:..."
      }
    }
  },
  "plugins": {
    "cyrene.models.hf-analyzer": {
      "version": "1.2.0",
      "capability": "model.analyzer.v1",
      "artifact": "ghcr.io/dohorizon-ai/plugins/hf-analyzer:1.2.0@sha256:..."
    }
  }
}
```

---

## 4. Distribution Profiles

- **`core`**: Platform Kernel Daemon, Node Agent, and foundational Control Plane.
- **`training`**: `core` + `Cyrene-Yield` + Preflight + training engine plugins.
- **`serving`**: `core` + `cyrene-reactor` + serving runtime plugins.
- **`gateway`**: `core` + `cyrene-exchange` + gateway runtime plugins.
- **`full`**: Complete verified matrix of all supported products and official plugins.
