# Recommended GitHub Branch Protection & Required CI Checks

This document provides recommended branch protection configurations for GitHub repository administrators.

---

## Recommended Status Checks by Repository

### 1. `Cyrene-Platform` (Target branch: `develop` / `main`)
- `ci / docs` (Markdown link and document index validation)
- `ci / governance` (Service boundary guard and dependency direction check)
- `ci / python-sdk` (Preflight, Control Plane, Artifacts, and Environment SDK tests)
- `ci / rust-kernel` (Cargo fmt, clippy, and unit tests with `--locked`)

### 2. `Cyrene-Plugins` (Target branch: `main`)
- `ci / docs` (Documentation and manifest schema syntax validation)
- `ci / conformance` (Capability Conformance TCK and Catalog evidence verification)
- `ci / python-plugins` (Lightweight unit tests for verified Python plugins)

### 3. `Cyrene-Yield` (Target branch: `main` / `develop`)
- `ci / governance` (Boundary guard validation)
- `ci / training-unit` (Training controller, spec compilation, and mock dry-run tests)

### 4. `Cyrene-Reactor` (Target branch: `main` / `develop`)
- `ci / rust-clippy-test` (Inference server Rust core tests)
- `ci / python-serving` (Serving runtime and API tests)

### 5. `Cyrene-Exchange` (Target branch: `main` / `develop`)
- `ci / gateway-tests` (Coordinator and routing tests)
