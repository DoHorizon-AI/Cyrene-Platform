# Dependency Management & Dependabot Guide

This guide explains how to add dependencies, update lockfiles, and manage automated Dependabot updates.

---

## 1. Updating Python Dependencies with `uv`

When updating dependencies in `Cyrene-Platform` SDKs or isolated plugins:
```bash
# Add a dependency
uv add httpx

# Update lockfile reproducibly
uv lock

# Verify lockfile matches environment
uv sync --locked
```

---

## 2. Updating Rust Dependencies with `cargo`

```bash
# Check dependencies without updating lockfile
cargo check --locked

# Update a specific crate
cargo update -p serde

# Verify formatting and clippy
cargo fmt --check
cargo clippy --workspace --all-targets
```

---

## 3. Dependabot Schedule & Grouping

- Dependabot is configured to check for updates **weekly**.
- Routine patch and minor updates are grouped to prevent PR noise.
- Security alerts trigger immediate standalone updates.
