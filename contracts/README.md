# Contracts

`contracts/` is the only cross-repository dependency boundary.

- `proto/` defines control and plugin RPC messages.
- `schemas/` defines manifests and canonical resources.
- `rust/` contains Rust mirrors and generated bindings.

Advanced services may depend on released contract artifacts or a checked-out
core repository. They must not depend on private paths inside the kernel or
framework implementations.

The existing Protobuf package name `cy.llm` is retained as a version-0 wire
compatibility namespace. It does not define the current product name. Changing
it requires a versioned protocol migration and coordinated regeneration in both
repositories.
