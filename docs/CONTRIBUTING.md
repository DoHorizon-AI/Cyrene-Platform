# Contributing during the private redesign

The core is currently developed privately. Keep changes inside one of the public
boundaries: `kernel/`, `framework/`, `contracts/`, or `examples/`.

Do not add first-party service behavior to this repository. Catalyst, Yield,
Reactor, Exchange, Navigator, Echo, vendor implementations, and enterprise
bundles belong in the advanced-services repository.

Before handing off a core change, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked --release
```

The JVM example has a separate compatibility check because it needs a Java
toolchain and Maven-generated protobuf sources:

```bash
mvn --batch-mode --file examples/plugins/jvm/poc/pom.xml package
CYRENE_REQUIRE_JVM_IT=1 cargo test -p cy-extension-registry --test jvm_integration_test --locked -- --nocapture
```

The JVM check may be skipped during local Rust-only development when the Java
toolchain is unavailable, but CI runs it as a required job.

Contract changes require a versioning decision and a compatibility test before
an advanced service may consume them.

Do not add a Python workspace, Python tests, package lock, container image, or
container-runtime probe to the core. Language-specific plugin code and service
packaging belong in the private advanced-services repository.
