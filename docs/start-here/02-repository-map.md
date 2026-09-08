# Platform Ownership Map

Cyrene-Platform owns only the generic foundation described by
[`repository-policy.yaml`](../../repository-policy.yaml): versioned contracts,
the Rust Kernel, process isolation and resource mechanisms, product-neutral
execution control, Platform daemon units, and repository-local CI.

It does not own Product lifecycle state, concrete AI engines, connector
adapters, deployment templates, compatibility snapshots, or cross-repository
catalogs. A new Product, plugin, or adapter must consume released Platform
contracts without adding its identity to this repository.

The current multi-repository map, target branches, and dependency pins are
mutable integration data owned by
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace). Consult
that repository instead of copying its catalog here.
