# Contributing during the private redesign

The core is currently developed privately. Keep changes inside one of the public
boundaries: `kernel/`, `framework/`, `contracts/`, `adapters/`, or `examples/`.

Do not add first-party service behavior to this repository. Catalyst, Yield,
Reactor, Exchange, Navigator, Echo, vendor implementations, and enterprise
bundles belong in the advanced-services repository.

## Development branches

- `develop-kernel` owns the Rust Kernel, Node Runtime, external hardware
  adapters, and their contract/code-generation changes.
- `develop-framework` owns Kotlin Framework and Control Plane work.
- `develop` is the reviewed integration branch for cross-language changes.
- `main` is a protected release branch and accepts only reviewed pull requests
  from `develop`.

Create short-lived feature or fix branches from the relevant development lane,
then merge the lane into `develop` through review. Core changes must preserve
versioned contract ownership and must not add legacy version-0 protocol or
agent-service references.

Before handing off a core change, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked --release
bash tooling/ci/check-kernel-boundary.sh
```

Contract changes require a versioning decision and a compatibility test before
an advanced service may consume them.

Installable business plugins must run out of process. Do not add in-proc-rust,
PyO3, model runtimes, or CUDA/ROCm compute libraries to a plugin manifest or
the Core runtime. A hardware adapter is a separately supervised process, not
an installable plugin and not a Kernel crate. It may contain a vendor C ABI
wrapper, but the Kernel communicates with it only through the versioned
`cyrene.hardware.v1` UDS protocol.

Do not add a Python workspace, Python tests, package lock, container image, or
container-runtime probe to the core. Language-specific plugin code and service
packaging belong in the private advanced-services repository.
