# CYRENE Platform

Cyrene-Platform is the reusable Rust Kernel, node/runtime agents, generic
control-plane framework, and language-neutral contract repository for the
CYRENE software family.

It contains no Catalyst, Yield, Reactor, Exchange, Navigator, Echo, or Plugin
business logic. Each Product and reusable capability lives in its own owning
repository and consumes versioned Platform contracts.

## Repository layout

| Directory | Responsibility |
| --- | --- |
| `kernel/` | Node-local semantic authority, Lease/Fence, lifecycle, and authenticated adapter clients |
| `agents/` | Generic Node and Runtime agents |
| `adapters/` | Platform-owned sandbox and hardware adapter hosts |
| `framework/` | Generic execution, workspace, Artifact, and Plugin control-plane services |
| `contracts/` | Kernel/control Protobuf, generic JSON Schemas, and Rust projections |
| `sdk/` | Provider-neutral Artifact and Platform client SDKs |
| `infrastructure/` | systemd units for Platform-owned daemons |
| `tooling/` | Repository-local CI, generation, and boundary checks |
| `docs/` | Normative architecture and operating guidance |

Product deployment templates, Product manifests, capability payload schemas,
Plugin repository manifests, ecosystem catalogs, and compatibility snapshots
are external. Adding a Product or Plugin method must not require a Platform
source change.

## Branches

`develop` is the reviewed integration branch and `main` is the protected release
branch. Current repository policy and CI configuration are authoritative.

## Key contracts

- [Platform clean boundary](docs/governance/platform-clean-boundary.md)
- [Kernel semantic contract](docs/contracts/kernel-semantic-contract-v1.md)
- [Platform contract index](docs/api/CAPABILITY_INDEX.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Repository boundaries](docs/REPOSITORY_BOUNDARIES.md)
