# CYRENE Core

CYRENE Core is the Rust host runtime and framework-contract repository for the
CYRENE AI software matrix. It is being developed privately first and is intended
to become the open-source trust base later.

This repository deliberately contains no Catalyst, Yield, Reactor, Exchange,
Navigator, or Echo product logic. Those products and other closed plugins live
in the private advanced-services repository and depend on versioned contracts
from this repository.

Development is split into language-specific lanes and converges through
`develop`:

- `develop-kernel` is the Rust Kernel/Node Runtime development branch.
- `develop-framework` is the Kotlin Framework/Control Plane development branch.
- `develop` is the reviewed integration branch for cross-language changes.

`main` is a protected release branch and accepts only reviewed pull requests
from `develop` after the required Core CI, architecture-governance, and
release-evidence checks pass.

## Repository layout

| Directory    | Responsibility                                                                                             |
| ------------ | ---------------------------------------------------------------------------------------------------------- |
| `kernel/`    | Rust supervisor, local transport, node agent, resource observation, sandbox and process lifecycle boundary |
| `framework/` | Extension API, registry, and JVM boundary                                                                  |
| `contracts/` | Protobuf, JSON Schema, canonical manifests, and generated protocol crates                                  |
| `examples/`  | Non-production plugin integration examples                                                                 |
| `docs/`      | Architecture, protocol, and repository-boundary decisions                                                  |

The former Rust control plane contained service-level training, runtime,
artifact, and serving implementations, so it is preserved in the private
advanced-services migration archive rather than published as core. The target
framework may evolve toward Kotlin/JVM, but it must continue to consume the
language-neutral contracts in `contracts/`; business behavior must not move
back into the kernel.

service.json describes a first-party service bundle. plugin.toml describes an
installable component inside the signed package; these are metadata levels, not
two package or installation protocols.

## Hardware boundary

The Rust Kernel keeps only node-local leases, fencing, lifecycle decisions and
generic local-adapter clients. Privileged cgroup/device/process execution runs
in the separately supervised `adapters/execution/sandboxd`; vendor discovery,
telemetry, device-node enumeration, and C ABI loading run in separate hardware
adapter processes under `adapters/hardware/`. CUDA, ROCm, Ascend, model
execution, training, and quantization stay outside the Kernel. Kernel and all
local adapters exchange versioned Protobuf frames over UDS; a C ABI may exist
only inside an adapter process, never as a public Kernel API.

## Current checkpoint

This repository is a clean-history core candidate assembled from audited source.
It excludes legacy products, backups, generated build output, local tooling
state, and private service implementations. Preserved legacy applications in
the advanced-services repository are not expected to build until their imports
are migrated to released core contracts.

See [architecture](docs/ARCHITECTURE.md) and
[repository boundaries](docs/REPOSITORY_BOUNDARIES.md).
