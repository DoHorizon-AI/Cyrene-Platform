# Your First Development Task

Welcome to Cyrene! This guide walks you through setting up your local development workspace, understanding the repository layout, making a verified change, and following the governance workflow.

---

## 1. Prerequisites & Toolchain Verification

Cyrene leverages multi-language runtimes for optimal performance. Ensure the following tools are installed:

- **Git** (>= 2.40)
- **Python** (>= 3.11, recommend Python 3.12) & `uv` (recommended fast package manager)
- **Rust** (>= 1.80) & `cargo`
- **.NET SDK** (>= 8.0 / 10.0, optional for gateway/internal services)
- **JDK** (>= 17, optional for JVM gateway)

Run the workspace doctor in `Cyrene-Platform`:
```bash
python tooling/workspace/workspace.py doctor
```

---

## 2. Inspecting the Workspace

Check the status of all local repositories in the Cyrene matrix:
```bash
python tooling/workspace/workspace.py status
```

---

## 3. Standard Contribution Cycle

1. **Pick an Issue / Define Task**: Ensure your change aligns with the [Where Does My Code Go?](03-where-does-my-code-go.md) guide.
2. **Create a Topic Branch**:
   ```bash
   git checkout -b feat/your-feature-name
   ```
3. **Implement Cleanly**:
   - Adhere to the [Cardinal Dependency Rule](../governance/repository-model.md) (`PRIVATE -> PUBLIC` allowed, `PUBLIC -> PRIVATE` forbidden).
   - Do not add product semantics to Kernel.
4. **Run Unit & Governance Tests**:
   - For Platform: `python -m pytest tooling/ci/test_check_service_boundaries.py`
   - For Plugins: `python -m pytest conformance/tests/`
   - For Yield / Reactor: run their respective pytest / cargo test suites.
5. **Open a Pull Request**: Fill out the PR template, noting any architecture impacts.
