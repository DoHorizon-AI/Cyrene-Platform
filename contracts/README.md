# Contracts

`contracts/` is the only cross-repository dependency boundary.

- `proto/` defines control and plugin RPC messages.
- `schemas/` defines manifests and canonical resources.
- `rust/` contains Rust mirrors and generated bindings.
- `tck/` contains language-neutral protocol vectors and reference conformance
  runners; plugin implementations must consume these rather than inventing
  worker-control semantics.

Advanced services may depend on released contract artifacts or a checked-out
core repository. They must not depend on private paths inside the kernel or
framework implementations.

Core RPCs live under the versioned `cyrene.core.v1` Proto module. The Rust
`cy-proto` crate exposes those generated types at `cyrene::core::v1` and keeps
generation in `OUT_DIR`; the checked-in descriptor and fixtures are the
cross-language compatibility baseline.

The former version-0 package and agent service were removed in the P1 cutover.
Runtime source and contract source must not reintroduce those names. The
separate `cy.plugin.v1` protocol remains the local out-of-process plugin stdio
contract and is not part of Core RPC.
