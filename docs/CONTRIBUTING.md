# Contributing

Keep Platform changes within generic Kernel, agent, adapter-host, framework,
Artifact, and control-plane boundaries. Product behavior, capability payloads,
Plugin implementations, Product deployment, and compatibility snapshots belong
to their owner repositories.

A new Platform API requires either two independent consumers or a Kernel-level
invariant plus a generic contract test. Product-specific convenience is not a
Platform gap.

Before handoff, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked --all-targets
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
python tooling/ci/verify.py --scope all-light
```

Installable business code runs out of process. Platform may supervise a
verified package and publish its opaque endpoint; it must not import the
implementation or inspect capability payloads.
