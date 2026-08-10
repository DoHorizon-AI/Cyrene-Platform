# Contracts

`contracts/` is the only cross-repository dependency boundary. The normative
semantic authority is [Kernel Semantic Contract v1](../docs/contracts/kernel-semantic-contract-v1.md);
wire schemas and language libraries are projections of that authority.

- `proto/cyrene/semantic/v1/` defines the language-neutral Kernel vocabulary.
- the remaining `proto/` packages define transport-specific control and plugin
  messages that import or map to that vocabulary.
- `schemas/` defines manifests and canonical resources.
- `rust/` contains Rust mirrors and generated bindings.
- `tck/kernel-semantic/v1/` contains the Python/Kotlin/Rust acceptance vectors
  for revision negotiation, validation, matching, lifecycle, authority and
  event replay. `tck/worker-control/v1/` separately covers the live Worker
  control channel.

Advanced services may depend on released contract artifacts or a checked-out
core repository. They must not depend on private paths inside the kernel or
framework implementations.

Compatibility Core RPCs live under the versioned `cyrene.core.v1` Proto module. The Rust
`cy-proto` crate exposes those generated types at `cyrene::core::v1` and keeps
generation in `OUT_DIR`; the checked-in descriptor and fixtures are the
cross-language compatibility baseline.

New interfaces must use the nine semantic nouns from `cyrene.semantic.v1`.
`cy-kernel-contract` is the pure Rust projection; `cy-proto` is the Protobuf
projection. Neither projection may add hidden semantics of its own.

The frozen semantic model does not imply that every compatibility transport is
already conforming. `cyrene.core.v1`, Node Agent and Hardware Adapter messages
remain migration projections until they pass the Kernel semantic TCK and the
conformance conditions listed in the normative document.

The former version-0 package and agent service were removed in the P1 cutover.
Runtime source and contract source must not reintroduce those names. The
separate `cy.plugin.v1` protocol remains the local out-of-process plugin stdio
contract and is not part of Core RPC.
