# CYRENE Core

CYRENE Core is the host runtime, framework contract, and SDK repository for the
CYRENE AI software matrix. It is being developed privately first and is intended
to become the open-source trust base later.

This repository deliberately contains no Catalyst, Yield, Reactor, Exchange,
Navigator, or Echo product logic. Those products and other closed plugins live
in the private advanced-services repository and depend on versioned contracts
from this repository.

## Repository layout

| Directory | Responsibility |
| --- | --- |
| `kernel/` | Rust supervisor, local transport, node agent, resource observation, sandbox and process lifecycle boundary |
| `framework/` | Extension API, registry, SDKs, JVM boundary, and service-manifest tooling |
| `contracts/` | Protobuf, JSON Schema, canonical manifests, and generated protocol crates |
| `examples/` | Non-production plugin integration examples |
| `docs/` | Architecture, protocol, and repository-boundary decisions |

The former Rust control plane contained service-level training, runtime,
artifact, and serving implementations, so it is preserved in the private
advanced-services migration archive rather than published as core. The target
framework may evolve toward Kotlin/JVM, but it must continue to consume the
language-neutral contracts in `contracts/`; business behavior must not move
back into the kernel.

## Hardware boundary

The Rust kernel discovers devices, reads telemetry, maps permitted device nodes,
injects vendor visibility variables, and supervises worker processes. CUDA,
ROCm, Ascend, model execution, training, and quantization stay in out-of-process
plugins. Vendor-specific discovery is an adapter boundary, not a driver inside
the kernel.

## Current checkpoint

This repository is a clean-history core candidate assembled from audited source.
It excludes legacy products, backups, generated build output, local tooling
state, and private service implementations. Preserved legacy applications in
the advanced-services repository are not expected to build until their imports
are migrated to released core contracts.

See [architecture](docs/ARCHITECTURE.md) and
[repository boundaries](docs/REPOSITORY_BOUNDARIES.md).
