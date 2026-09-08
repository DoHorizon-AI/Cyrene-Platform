# How to Read Cyrene-Platform

Trace a Platform mechanism from its contract to its implementations and tests.

1. Start with `contracts/proto/` or `contracts/schemas/` to identify the
   versioned semantic boundary.
2. Read `kernel/crates/cy-kernel-api/` for Kernel types and
   `kernel/crates/cy-kernel/` for node-local lifecycle behavior.
3. Read `framework/crates/` for generic capability execution, package runtime,
   control-plane, and fabric mechanisms.
4. Read `sdk/` for language projections of those contracts.
5. Finish with the nearest TCK and repository boundary guard under
   `contracts/tck/` or `tooling/ci/`.

Ask which owner controls the state being changed. Node-local leases, fencing,
process supervision, transport, and immutable Artifact references are Platform
mechanisms. User intent, lifecycle policy, billing, routing decisions, concrete
engines, and vendor adapters belong to consumers or plugins.

Cross-repository workflow traces and current repository locations belong in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
