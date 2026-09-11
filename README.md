# CYRENE Platform

[![Rust](https://img.shields.io/badge/Rust-1.96.1%2B-orange.svg)](rust-toolchain.toml)
[![Platform Kernel](https://img.shields.io/badge/Kernel-Frozen_v1-blue.svg)](docs/contracts/kernel-semantic-contract-v1.md)
[![Core license](https://img.shields.io/badge/Core-AGPL--3.0--only-green.svg)](LICENSES/AGPL-3.0-only.txt)
[![Public interfaces](https://img.shields.io/badge/Public_interfaces-Apache--2.0-blue.svg)](LICENSES/Apache-2.0.txt)

Cyrene-Platform is the reusable Rust system substrate and kernel for the CYRENE
software family. It provides deterministic node-local resource ownership,
Lease/Fence correctness, process lifecycle governance, hardware fact exchange,
journaled recovery, and versioned control protocols.

It contains no Catalyst, Yield, Reactor, Exchange, Navigator, Echo, or Plugin
business logic. Products and reusable capabilities live in their owning
repositories and consume versioned Platform contracts.

## API Naming Constitution

Cyrene uses one shared semantic vocabulary across Platform, Products, and
Plugins. The normative source is
[`docs/governance/API_NAMING_CONSTITUTION.md`](docs/governance/API_NAMING_CONSTITUTION.md).
It fixes the distinctions between `Acquire`/`Reserve`, `Start`/`Launch`,
`Stop`/`Terminate`, `Watch`/`Subscribe`, and the `State`/`Status`/`Phase`
abstraction layers.

This repository is the language source for the whole Cyrene system. Every
breaking rename must update contract inputs, generated bindings, consumers,
fixtures, and state-machine evidence together. The migration gate is defined
by [`tooling/architecture/api-naming.toml`](tooling/architecture/api-naming.toml)
and runs through
[`tooling/ci/check-api-naming.py`](tooling/ci/check-api-naming.py). The public
API naming freeze begins with the first public release.

Cyrene 使用统一的 API 语义词汇；Platform 是整个系统的语言源头。首次公开
发布前可以进行 breaking rename，但必须同步更新合约、生成绑定、消费者、
fixture 与状态机证据。首次 public release 开始后，公共 API 命名冻结。

## Core design philosophy

Platform is a generic mechanism layer, not an AI product framework.

Platform owns:

- principal and node identity boundaries;
- generic resource inventory, allocation, leases, and monotonic fence tokens;
- process lifecycle, cgroup-scoped resource controls, cleanup, and runtime evidence;
- provider-neutral endpoints, capabilities, operations, and ordered events;
- versioned wire protocols and public contract projections.

Platform does not own Transformer architectures, prompts, token accounting, model
weights, training policy, product manifests, conversations, or business billing.
Those concerns belong to Products and independent extensions.

## System architecture

```text
Worker / Provider / Service / Hardware Adapter
                    |
                    | public contracts + versioned gRPC/UDS/IPC
                    v
        Apache-2.0 Public Contracts / SDK
                    |
=============================================================
          compile-time and runtime architecture boundary
=============================================================
                    |
                    v
             AGPL-3.0-only Platform Core
                    |
       +------------+----------------------+
       |                                   |
  Kernel authority                 cyrene-sandboxd
  Lease/Fence/journal              privileged Core service
```

An out-of-process boundary is not automatically an extension boundary.
`cyrene-sandboxd` remains Platform Core because it owns privileged cgroup,
device-policy, process-lifecycle, and cleanup enforcement.

## Repository layout

The Rust workspace currently contains 21 packages:

| Directory | Responsibility |
| --- | --- |
| `contracts/` | Protobuf, JSON schemas, semantic projections, and public contract facts |
| `kernel/` | Kernel authority, Lease/Fence/resource management, and internal IPC clients |
| `framework/` | Product-neutral execution and workspace control-plane implementation |
| `runtime/` | Kernel composition root and host runtime |
| `agents/` | Generic Node and Runtime agents |
| `adapters/` | Privileged sandbox and reference hardware adapter processes |
| `sdk/` | Public Rust Artifact SDK and Python SDK packages |
| `infrastructure/` | systemd units and deployment references |
| `tooling/` | CI gates, package checks, protocol mirror checks, and external consumers |
| `docs/` | Normative architecture, security, governance, and release guidance |

Adding a Product or Plugin method must not require a Platform source change.
Product deployment templates, capability payload schemas, plugin repository
manifests, ecosystem catalogs, and compatibility snapshots remain external.

## Quick start

### Prerequisites

The CPU-only workspace build and unit tests do not require a GPU, root access,
or a system `protoc`; `cy-proto` uses vendored `protoc`. Install the pinned Rust
toolchain with `rustup`:

```bash
rustup show
```

For the full host runtime, use Linux with cgroups v2 delegation and protected
Unix-domain sockets. NVIDIA operation additionally requires the vendor tools
and device access. Buf is required only for the Workspace/Protobuf wire checks;
CI installs it explicitly.

### Build and test

```bash
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

Repository governance and public-boundary checks use the locked Python
environment:

```bash
uv sync --locked
uv run --locked python tooling/ci/verify.py --scope all-light
uv run --locked python tooling/ci/check-license-boundary.py
uv run --locked python tooling/ci/check-license-metadata.py
bash tooling/ci/check-public-proto-sync.sh
bash tooling/ci/check-public-packages.sh
bash tooling/acceptance/licensing-boundary/run.sh
```

### Running the host runtime

There is no zero-configuration production daemon command. The kernel and
privileged services require explicit node, socket, adapter, installation-root,
journal, cgroup, and peer-identity configuration. Use the checked-in systemd
units as the production starting point:

- `infrastructure/systemd/cyrene-kernel.service`
- `infrastructure/systemd/cyrene-sandboxd.service`
- `infrastructure/systemd/cyrene-nvidia-adapter.service`

`cyrene-sandboxd --dev-mode` is a development convenience; it does not provide
hostile-code containment or a complete multi-tenant security boundary. See
[the security model](docs/security/threat-model.md) before operating the
privileged services.

## Public extension boundary

Supported independent extensions run out of process and use documented wire
contracts. They do not link Platform implementation crates.

| Extension | Transport | Compile-time dependency | License choice |
| --- | --- | --- | --- |
| Worker | Worker control IPC/UDS | `cy-proto` and optional public SDK | Author-selected |
| Provider | Versioned provider UDS/gRPC | `cy-kernel-contract` + `cy-proto` | Author-selected |
| Service plugin | Versioned service/control protocol | Public protocol/SDK only | Author-selected |
| Hardware adapter | Hardware Adapter v1 UDS | `cy-kernel-contract` + `cy-proto` | Author-selected |
| Sandbox | Privileged Core UDS | Core/internal APIs | Platform Core policy |
| Node/Runtime agent | Typed control protocol | Platform-owned implementation | Platform Core policy |

See the [machine-readable boundary policy](tooling/architecture/license-boundaries.toml),
the [independent consumer gate](tooling/acceptance/licensing-boundary/), and the
[public package release order](docs/release/PUBLIC_PACKAGE_RELEASE_ORDER.md).

## Documentation and onboarding

- [System map](docs/ARCHITECTURE.md)
- [Kernel semantic contract v1](docs/contracts/kernel-semantic-contract-v1.md)
- [Platform clean boundary](docs/governance/platform-clean-boundary.md)
- [Repository boundaries](docs/REPOSITORY_BOUNDARIES.md)
- [Capability contract index](docs/api/CAPABILITY_INDEX.md)
- [Security threat model](docs/security/threat-model.md)
- [Contribution guide](CONTRIBUTING.md)
- [Open-source architecture readiness](docs/governance/OPEN_SOURCE_ARCHITECTURE_READINESS.md)
- [Public release preflight](docs/governance/PUBLIC_RELEASE_PREFLIGHT.md)
- [API Naming Constitution](docs/governance/API_NAMING_CONSTITUTION.md)
- [API Naming Migration Plan](docs/governance/API_NAMING_MIGRATION_PLAN.md)

## Licensing

Cyrene-Platform uses a tiered licensing layout:

1. Platform Core implementation is `AGPL-3.0-only`.
2. Designated public contracts, protocol inputs, schemas, and SDKs are
   `Apache-2.0`.
3. Independently developed workers, plugins, providers, hardware adapters, and
   integrations that use only the designated public process seam may choose
   their own license, including proprietary commercial licenses.

This does not change the license applicable to modifications of Cyrene-Platform
Core itself. This is a description of the repository's technical and licensing
intent, not legal advice. Read [LICENSING.md](LICENSING.md) for the complete
mapping and contribution policy.

## Project status

The repository is pre-release. The local build, test, package, dependency
firewall, and independent-consumer gates are maintained here. Hosted CI and a
fresh Ubuntu lifecycle run remain release-preflight evidence and must not be
represented as local PASS results.
