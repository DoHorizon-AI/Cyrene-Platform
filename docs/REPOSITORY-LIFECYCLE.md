# Repository Lifecycle: Cyrene-Platform

Cyrene-Platform is classified as `PRIVATE_FOUNDATION` with private visibility.
It is an independently buildable trust-base component rather than a standalone
user Product.

## Ownership

The repository owns foundational Protobuf and schema contracts, the Rust
Kernel, process isolation and resource mechanisms, product-neutral execution
control, Platform daemon units, and repository-local CI.

It does not own Product lifecycle state, concrete AI engines, Product
infrastructure, edge configuration, adapter implementations, compatibility
snapshots, or cross-repository Product and plugin catalogs.

## Build and release

- Independent build: `true`
- Integration branch: `develop`
- Release branch: `main`
- Release role: `COMPONENT_RELEASE`
- Versioning: repository-scoped SemVer with immutable `v{version}` tags
- Hosted CI authority: `azure_devops`
- Deployment authority: `consuming_repository`
- Multi-repository build required: `false`

The machine-readable authority is [`repository-policy.yaml`](../repository-policy.yaml).
Platform builds and source gates must not discover sibling repositories.
Multi-repository topology and compatibility pins belong to
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
