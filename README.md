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
- [System Adapter contract and Linux implementation](docs/architecture/system-adapter.md)
- [Sandbox Adapter boundary and backend status](docs/architecture/sandbox-adapter.md)
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
- [Chinese documentation set](docs/zh-CN/README.md)

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
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Platform

Cyrene-Platform 是 CYRENE 软件家族通用的 Rust 系统基座与内核。它提供确定性的节点本地资源归属、Lease/Fence 正确性、进程生命周期治理、硬件事实交换、带日志的恢复机制以及版本化控制协议。

本仓库不包含 Catalyst、Yield、Reactor、Exchange、Navigator、Echo 或 Plugin 的业务逻辑。各 Product 与可复用能力实现由其所属仓库负责，并消费版本化 Platform 契约。

## API 命名宪法

Cyrene 在 Platform、Products 与 Plugins 之间共用一套语义词汇。规范来源是 docs/governance/API_NAMING_CONSTITUTION.md。该文档明确区分 Acquire/Reserve、Start/Launch、Stop/Terminate、Watch/Subscribe，以及 State/Status/Phase 各抽象层的含义。

本仓库是整个 Cyrene 系统的语言源头。任何破坏性重命名都必须同时更新契约输入、生成绑定、消费者、fixtures 与状态机证据。迁移门禁由 tooling/architecture/api-naming.toml 定义，并通过 tooling/ci/check-api-naming.py 执行。公共 API 命名冻结从首次公开发布开始。

Cyrene 使用统一的 API 语义词汇；Platform 是整个系统的语言源头。首次公开发布前可以进行 breaking rename，但必须同步更新契约、生成绑定、消费者、fixture 与状态机证据。首次 public release 开始后，公共 API 命名冻结。

## 核心设计理念

Platform 是通用机制层，不是 AI Product 框架。

Platform 拥有：

- principal 与 node 身份边界；
- 通用资源清单、分配、租约和单调递增的 fence token；
- 进程生命周期、cgroup 资源控制、清理与运行证据；
- 与提供方无关的 endpoint、capability、operation 和有序事件；
- 版本化 wire 协议与公开契约投影。

Platform 不拥有 Transformer 架构、prompt、token 计量、模型权重、训练策略、Product manifest、对话或业务计费。这些职责属于 Products 与独立扩展。

## 系统架构

```text
Worker / Provider / Service / Hardware Adapter
                    |
                    | 公开契约 + 版本化 gRPC/UDS/IPC
                    v
        Apache-2.0 公开契约 / SDK
                    |
=============================================================
             编译期与运行时架构边界
=============================================================
                    |
                    v
             AGPL-3.0-only Platform Core
                    |
       +------------+----------------------+
       |                                   |
  Kernel authority                 cyrene-sandboxd
  Lease/Fence/journal              特权 Core service
```

进程外边界并不自动成为扩展边界。cyrene-sandboxd 仍属于 Platform Core，因为它负责特权 cgroup、设备策略、进程生命周期和清理强制执行。

## 仓库布局

当前 Rust workspace 包含 21 个 package：

| 目录 | 职责 |
| --- | --- |
| contracts/ | Protobuf、JSON schema、语义投影与公开契约事实 |
| kernel/ | Kernel authority、Lease/Fence/资源管理和内部 IPC client |
| framework/ | 与 Product 无关的执行和 workspace 控制平面实现 |
| runtime/ | Kernel composition root 与 host runtime |
| agents/ | 通用 Node 与 Runtime agent |
| adapters/ | 特权 sandbox 与参考硬件适配器进程 |
| sdk/ | 公开 Rust Artifact SDK 与 Python SDK package |
| infrastructure/ | systemd unit 与部署参考 |
| tooling/ | CI 门禁、package 检查、协议镜像检查与外部消费者 |
| docs/ | 规范架构、安全、治理与发布指南 |

增加 Product 或 Plugin 方法不得要求修改 Platform 源码。Product 部署模板、capability payload schema、plugin repository manifest、生态目录和兼容性快照仍由外部仓库维护。

## 快速开始

### 前置条件

仅 CPU 的 workspace 构建与单元测试不需要 GPU、root 权限或系统级 protoc；cy-proto 使用 vendored protoc。使用 rustup 安装固定的 Rust toolchain：

```bash
rustup show
```

运行完整 host runtime 需要 Linux、cgroups v2 delegation 和受保护的 Unix-domain socket。NVIDIA 操作还需要厂商工具及设备访问权限。Workspace/Protobuf wire 检查才需要 Buf；CI 会显式安装它。

### 构建与测试

```bash
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

仓库治理与公开边界检查使用锁定的 Python 环境：

```bash
uv sync --locked
uv run --locked python tooling/ci/verify.py --scope all-light
uv run --locked python tooling/ci/check-license-boundary.py
uv run --locked python tooling/ci/check-license-metadata.py
bash tooling/ci/check-public-proto-sync.sh
bash tooling/ci/check-public-packages.sh
bash tooling/acceptance/licensing-boundary/run.sh
```

### 运行 host runtime

生产 daemon 没有零配置启动命令。Kernel 与特权 service 需要显式配置 node、socket、adapter、installation root、journal、cgroup 和 peer identity。生产部署请以仓库中的 systemd unit 为起点：

- infrastructure/systemd/cyrene-kernel.service
- infrastructure/systemd/cyrene-sandboxd.service
- infrastructure/systemd/cyrene-nvidia-adapter.service

cyrene-sandboxd --dev-mode 只是开发便利选项；它不提供恶意代码隔离，也不构成完整的多租户安全边界。运行特权 service 前请阅读安全模型 docs/security/threat-model.md。

## 公开扩展边界

受支持的独立扩展在进程外运行并使用文档化 wire 契约，不链接 Platform 实现 crate。

| 扩展 | 传输 | 编译期依赖 | 许可证选择 |
| --- | --- | --- | --- |
| Worker | Worker control IPC/UDS | cy-proto 和可选公开 SDK | 作者自行选择 |
| Provider | 版本化 Provider UDS/gRPC | cy-kernel-contract + cy-proto | 作者自行选择 |
| Service plugin | 版本化 service/control 协议 | 仅公开协议/SDK | 作者自行选择 |
| Hardware adapter | Hardware Adapter v1 UDS | cy-kernel-contract + cy-proto | 作者自行选择 |
| Sandbox | 特权 Core UDS | Core/内部 API | 遵循 Platform Core 政策 |
| Node/Runtime agent | 有类型的控制协议 | Platform 所有实现 | 遵循 Platform Core 政策 |

参见机器可读边界政策 tooling/architecture/license-boundaries.toml、独立消费者门禁 tooling/acceptance/licensing-boundary/ 和公开 package 发布顺序 docs/release/PUBLIC_PACKAGE_RELEASE_ORDER.md。

## 文档与上手指南

- 系统地图：docs/ARCHITECTURE.md
- System Adapter 契约与 Linux 实现：docs/architecture/system-adapter.md
- Sandbox Adapter 边界与后端状态：docs/architecture/sandbox-adapter.md
- Kernel 语义契约 v1：docs/contracts/kernel-semantic-contract-v1.md
- Platform 清晰边界：docs/governance/platform-clean-boundary.md
- 仓库边界：docs/REPOSITORY_BOUNDARIES.md
- Capability 契约索引：docs/api/CAPABILITY_INDEX.md
- 安全威胁模型：docs/security/threat-model.md
- 贡献指南：CONTRIBUTING.md
- 开源架构准备情况：docs/governance/OPEN_SOURCE_ARCHITECTURE_READINESS.md
- 公开发布预检：docs/governance/PUBLIC_RELEASE_PREFLIGHT.md
- API 命名宪法：docs/governance/API_NAMING_CONSTITUTION.md
- API 命名迁移计划：docs/governance/API_NAMING_MIGRATION_PLAN.md
- 中文文档集：docs/zh-CN/README.md

## 许可布局

Cyrene-Platform 采用分层许可布局：

1. Platform Core 实现使用 AGPL-3.0-only。
2. 指定的公开契约、协议输入、schema 与 SDK 使用 Apache-2.0。
3. 独立开发的 worker、plugin、provider、硬件 adapter 与集成，只要仅使用指定的公开进程接缝，即可自行选择许可证，包括专有商业许可证。

这不会改变适用于 Cyrene-Platform Core 自身修改的许可证。以上内容描述仓库的技术与许可意图，不构成法律意见。完整映射与贡献政策见 LICENSING.md。

## 项目状态

仓库处于预发布阶段。本地构建、测试、package、依赖防火墙与独立消费者门禁由本仓库维护。托管 CI 和全新的 Ubuntu 生命周期运行仍属于发布预检证据，不得表述为本地 PASS。
