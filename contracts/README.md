# Contracts

`contracts/` is the only cross-repository dependency boundary.

- `proto/` defines control and plugin RPC messages.
- `schemas/` defines manifests and canonical resources.
- `rust/` contains Rust mirrors and generated bindings.

Advanced services may depend on released contract artifacts or a checked-out
core repository. They must not depend on private paths inside the kernel or
framework implementations.

The existing Protobuf package name `cy.llm` is an internal version-0
migration artifact. P0 does not add or change its RPCs, messages, fields,
commands, or consumers. New code must not depend on it. P1 will replace it with
a versioned Core contract and delete the legacy Proto and generated binding in
one reviewed cutover.
