# Contributing during the private redesign

The core is currently developed privately. Keep changes inside one of the public
boundaries: `kernel/`, `framework/`, `contracts/`, or `examples/`.

Do not add first-party service behavior to this repository. Catalyst, Yield,
Reactor, Exchange, Navigator, Echo, vendor implementations, and enterprise
bundles belong in the advanced-services repository.

Use develop for daily changes. main is a protected release branch and accepts
only reviewed pull requests from develop. A pull request that changes the
legacy cy.llm contract or adds a new consumer is not valid P0 work.

Before handing off a core change, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
```

Contract changes require a versioning decision and a compatibility test before
an advanced service may consume them.

Installable business plugins must run out of process. Do not add
in-proc-rust, PyO3, model runtimes, or CUDA/ROCm compute libraries to a plugin
manifest or the Core runtime. Static Rust platform adapters are internal Core
implementation details, not installable plugins.

Do not add a Python workspace, Python tests, package lock, container image, or
container-runtime probe to the core. Language-specific plugin code and service
packaging belong in the private advanced-services repository.
