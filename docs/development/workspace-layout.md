# Canonical Cyrene Workspace Layout & Bootstrapping

This document defines the canonical directory structure, multi-repository layout, and bootstrapping workflow for the Cyrene platform.

---

## 1. The Canonical Workspace Topology

$$\mathbf{Cyrene/\ \text{is a workspace container, NOT a Git repository.}}$$

```text
Cyrene/                       # Top-level workspace container (NO root .git, NO submodules)
├── Cyrene-Platform/          # Independent Git repo: Contracts, Rust Kernel, Control Plane, SDKs, Tooling
├── plugins/                  # Independent Git repo (Cyrene-Plugins-Official): Capability implementations
└── services/                 # Independent Service repositories container
    ├── Cyrene-Yield/         # Independent Git repo: Model Training Product
    ├── cyrene-reactor/       # Independent Git repo: Model Serving & Inference Product
    ├── cyrene-exchange/      # Independent Git repo: API Gateway & Load Balancer
    ├── cyrene-astrbot-rev/   # Independent Git repo: Agent Hub & Connectors
    ├── cyrene-catalyst/      # Independent Git repo: Dialogue auxiliary service
    ├── cyrene-echo/          # Independent Git repo: Audio realtime service
    ├── cyrene-navigator/     # Independent Git repo: Desktop GUI & Client
    └── cyrene-dh-system-internal/ # (Optional Private Overlay) Enterprise WeCom Hub
```

> [!IMPORTANT]
> The outer `Cyrene/` folder must **NEVER** be initialized as a Git repository (`git init`) and must **NEVER** use Git submodules. Each child repository manages its own Git history, branches, and remotes.

---

## 2. Bootstrapping a Developer Workspace

A new engineer only needs to clone `Cyrene-Platform` initially:

```bash
# 1. Create workspace root
mkdir Cyrene
cd Cyrene

# 2. Clone Cyrene-Platform
git clone https://github.com/DoHorizon-AI/Cyrene-Platform.git Cyrene-Platform

# 3. Hydrate dependencies for desired profile (e.g. training)
python Cyrene-Platform/tooling/workspace/workspace.py bootstrap --profile training

# 4. Verify workspace health
python Cyrene-Platform/tooling/workspace/workspace.py doctor
python Cyrene-Platform/tooling/workspace/workspace.py status
```

---

## 3. Developer Safety Invariants

1. **Zero Destructive Overwrites**: `workspace.py bootstrap` / `hydrate` will **NEVER** run `git reset`, `git clean`, `git checkout -f`, or discard uncommitted changes in existing repositories.
2. **Dirty Repository Protection**: If a repository exists with uncommitted changes, it is reported as `DIRTY` and left untouched.
3. **Optional Private Overlays**: If a developer lacks access to private repositories (`cyrene-dh-system-internal`), the workspace remains 100% functional for public Community development.
