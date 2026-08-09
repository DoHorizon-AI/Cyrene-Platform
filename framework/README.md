# Framework

The framework is the language-friendly control and extension layer above the
kernel. It consumes the contracts in `../contracts` and asks the kernel to
enforce lifecycle and resource decisions.

The Rust crates in this directory provide the public platform API and extension
registry, not first-party service implementations. New public APIs must be
protocol-first so a future Kotlin/JVM framework can coexist with or replace the
current orchestration layer without changing plugins or the Rust kernel.

Advanced services are discovered through manifests. The framework must never
hard-code one of the six CYRENE products.
