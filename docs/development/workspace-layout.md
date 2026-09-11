# Platform Checkout Boundary

Cyrene-Platform is an independently buildable repository. Its build, tests, and
release tooling discover only files in this checkout and do not clone, inspect,
or mutate sibling repositories.

## Platform checkout

```text
Cyrene-Platform/
├── adapters/        # Out-of-process hardware and sandbox adapters
├── agents/          # Platform node agents
├── contracts/       # Versioned wire and schema contracts
├── framework/       # Generic runtime and language boundaries
├── infrastructure/  # Units for Platform-owned daemons only
├── kernel/          # Rust Kernel implementation
├── sdk/             # Platform client SDKs
└── tooling/         # Repository-local CI, codegen, release, and runtime tools
```

Run the local verification entry point from this repository:

```bash
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

Multi-repository layout, checkout profiles, dependency pins, and status tooling
are owned by [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
Changing that topology does not require a Platform change.
