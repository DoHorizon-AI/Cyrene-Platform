# Open-Source Architecture Readiness

本报告记录候选整改阶段的架构事实与可验证性。它不是法律意见；正式
许可证布局现已在 canonical `develop` 中由 `LICENSES/`、`LICENSING.md`
和 package metadata 表达。

## 1. Baseline

| Item | Evidence |
| --- | --- |
| Branch | `develop` in the canonical checkout; current live HEAD is the promoted local adapter boundary commit. |
| HEAD before current adapter promotion | `7b279d237161d8a6181c80d78b780c61af0fd548` |
| HEAD after current adapter promotion | `8a6a6c0bde136f474ffb15b39dba2619db9157bf`; committed locally and read back on canonical `develop`. |
| Task worktree | `/tmp/cyrene-finalize-platform-adapters`; clean and points at the same commit. |
| Canonical checkout | `/home/baijin/Dev/Cyrene/Cyrene-Platform`; clean on `develop`, with the promoted commit intentionally not pushed. |
| PR #41 | Merge commit `dc37791a806bd6c405932f0f0457518db6e6aaa0` remains an ancestor of the current HEAD. |
| Worktree after cleanup | Intended architecture, adapter, documentation, and systemd changes are committed on `develop`; no unrelated canonical changes were taken over. |

The previous licensing-audit baseline `f56a7f30c31a729a7d4c030813356018a7be3dc1`
is historical evidence only. The current live state is `develop@8a6a6c0`; PR #41
is also historical lineage evidence, and its hosted check rollup is not treated
as a current green acceptance result.

## 2. Ground-truth inventory and classification

Current repository facts are 21 Rust workspace packages, 2 Python SDK packages,
and 15 canonical Protobuf files under `contracts/proto`. The old “16 core
crates” wording is stale. The old `cy-plugin-sdk` and `cy-plugin-protocol`
paths remain absent after PR #41. No existing `examples/hello-worker` or
equivalent full lease-to-cleanup demo was found; the new independent consumers
are compile-boundary tests, not a fake lifecycle demo.

| Component | Architecture classification | Boundary disposition |
| --- | --- | --- |
| `contracts/rust/cy-kernel-contract` | `PUBLIC_CONTRACT` | Public semantic vocabulary plus implementation-free adapter facts/ports. |
| `contracts/rust/cy-proto` | `PUBLIC_CONTRACT` | Versioned wire contracts and generated bindings. |
| `contracts/rust/cy-manifest` | `PUBLIC_CONTRACT` | Generic manifest and identity contract. |
| `sdk/rust/cy-artifact-transfer` | `PUBLIC_SDK` | Provider-neutral Artifact transfer SDK. |
| `framework/crates/cy-execution-fabric` | `PUBLIC_SDK` | Product-neutral admission/attachment/reconciliation API using public contracts. |
| `framework/crates/cy-workspace-fabric` | `PUBLIC_SDK` | Public/client-facing relay API; no longer imports execution-fabric implementation types. |
| `sdk/python/cyrene_artifacts` | `PUBLIC_SDK` | Provider-neutral Python Artifact interface. |
| `sdk/python/cyrene_preflight` | `PUBLIC_SDK` | Public resource-fact/preflight interface. |
| `cyrene-linux-sys-adapter`, `cyrene-nvidia-adapter` | `OUT_OF_PROCESS_ADAPTER` / `SEPARATE_DECISION` | Reference implementations; compile only against public `cy-kernel-contract` and `cy-proto`. |
| `cyrene-sandboxd` | `PRIVILEGED_PLATFORM_SERVICE` / `CORE_IMPLEMENTATION` | Out of process, but Core: cgroups, BPF device policy, process ownership, pidfd/reaping, and cleanup. |
| `cyrene-kernel` | `CORE_IMPLEMENTATION` | Kernel composition root and authority runtime. |
| `cy-kernel-daemon`, `cy-resource-manager`, `cy-kernel-api` | `CORE_IMPLEMENTATION` / `INTERNAL_HOST_API` | Lease/fence authority, resource accounting, internal ports and orchestration. |
| `cy-adapter-client`, `cy-sandbox-client` | `INTERNAL_LIBRARY` / `INTERNAL_HOST_API` | Kernel-side bounded UDS clients, not third-party SDKs. |
| `cy-execution-control`, `cy-installation-resolver` | `CORE_IMPLEMENTATION` | Core control/session and verified-installation handoff. |
| `cy-platform-api` | `INTERNAL_LIBRARY` | Registry, resolver, and repository-manifest normalization; the `api` name does not make it public. |
| `cy-package-runtime` | `CORE_IMPLEMENTATION` / `INTERNAL_HOST_API` | Package lifecycle, activation, and supervisor implementation. |
| `cy-node-agent`, `cy-runtime-agent` | `CORE_IMPLEMENTATION` | Platform agents using typed control protocols; neither is a second authority. |
| `tooling/architecture`, `tooling/ci`, `tooling/acceptance/licensing-boundary` | `TOOLING` / `TEST_ONLY` | Machine-verifiable boundary, package, mirror, and external-consumer gates. |
| Worker/provider/service plugin | `OUT_OF_PROCESS_PLUGIN` | Not a current workspace crate; supported through versioned wire contracts and public SDKs. |

## 3. Previous findings revalidated

| Previous finding | Current reality | Disposition |
| --- | --- | --- |
| Workspace had 21 Rust packages, 2 Python packages, 15 proto files | Still true at current HEAD; old 16-crate description is stale | Reported from live metadata, not old report counts. |
| PR #41 removed old plugin paths | Still true; merge commit is an ancestor and old crates are absent | No legacy path restored. |
| `cy-kernel-api` mixed public facts and internal ports | Confirmed at baseline; after cleanup the shared adapter facts have one canonical owner in `cy-kernel-contract`. `cy-kernel-api` remains a Core implementation-facing crate with a public compatibility surface; only selected exports are `doc(hidden)` and they remain callable | Internal ports and compatibility exports remain Core; neither `pub` nor `doc(hidden)` is the supported third-party extension API. |
| No `cy-kernel-api -> cy-resource-manager` reverse edge | Still true; current edge is `cy-resource-manager -> cy-kernel-api` | No unnecessary reverse dependency was introduced. |
| Independent consumer could use contracts/protocol without Core | Revalidated and strengthened with two detached Cargo consumers | Worker and Hardware consumer both compile and their normal/build trees contain no Core crate. |
| `cy-platform-api` looked public by name | Current code still contains registry/resolver/normalization logic | Classified and documented as internal; no forced split. |
| `cy-workspace-fabric::RelayClientConfig` leaked `ConnectivityRoute` | Confirmed and fixed | Config now owns endpoint/server-name/TLS primitives; normal dependency on `cy-execution-fabric` is gone. |
| `cy-package-runtime` mixed control formats and host lifecycle | Current implementation remains lifecycle/supervisor host logic | Kept internal; no unsupported public SDK was declared. |
| `SandboxBackend`, Worker transport, and raw invoke looked like plugin APIs | Current `SandboxBackend`/Worker transport remain Rust host internals; `InstanceActor::invoke_raw` is explicitly documented as an internal hook | No needless RPC rewrite; external support points to wire protocols. |
| Official hardware adapters imported `cy-kernel-api` | Confirmed before cleanup; now they import only public `cy-kernel-contract` facts and `cy-proto` | Third-party adapter compile closure no longer requires Kernel implementation. |
| `sandboxd` was out of process | Still true, but it owns privileged enforcement and lifecycle | Explicitly classified as `PRIVILEGED_PLATFORM_SERVICE`, not arbitrary-license plugin. |
| `cy-proto` package missed external proto inputs | Reproduced and fixed | Package-local proto mirror and core fixture mirror are checked against canonical sources. |
| Artifact SDK path dependencies lacked versions | Reproduced and fixed for its public path dependencies | Versioned path dependencies plus package verification patches. |
| README lacked clean-machine commands | Current clean task worktree README now has build/test, gates, and Linux/GPU/root expectations | Full zero-config daemon demo is still not claimed. |
| Sandbox was a complete hostile-code security sandbox | Current source still lacks namespaces, seccomp, and capability dropping | Security documentation now states resource/lifecycle guarantees and non-guarantees. |

## 4. Final compile dependency graph

This graph is compile-time only. Arrows are normal/build dependencies; dev-only
test dependencies are not part of the public package closure gate.

```text
PUBLIC CONTRACTS
  cy-kernel-contract  (semantic + adapter facts/ports)
  cy-proto            (versioned wire bindings)
  cy-manifest         (generic manifest contract)
        ^                  ^              ^
        |                  |              |
  cy-artifact-transfer    |       cy-execution-fabric
        ^                  |              ^
        +------------------+--------------+
                           |
                   cy-workspace-fabric

OFFICIAL / THIRD-PARTY ADAPTER SIDE
  hardware adapter -> cy-kernel-contract + cy-proto
  external worker  -> cy-proto / public SDK
  external provider/service -> public contract / SDK

CORE SIDE
  cy-kernel-api -> cy-kernel-contract
  cy-adapter-client -> cy-kernel-contract + cy-proto
  cy-resource-manager -> cy-kernel-api
  cy-kernel-daemon -> cy-kernel-api + cy-resource-manager + cy-adapter-client + cy-proto
  cyrene-kernel -> Core crates
  cyrene-sandboxd -> cy-kernel-api + cy-proto
  agents/control/runtime/package services -> Core and/or public contract crates
```

The machine-readable source of truth is
`tooling/architecture/license-boundaries.toml`. The gate runs full
`cargo metadata --locked --all-features`, traverses normal and build dependency
edges, and rejects any public-to-Core transitive path. It also checks public
Rust sources for Core crate references.

## 5. Final runtime boundary graph

```text
External worker / provider / service / hardware adapter
             |
             | versioned Protobuf, documented UDS/gRPC/IPC
             v
     Public Contracts / SDK
             |
             | explicit process boundary
             v
       Kernel Core authority
       (principal, lease, fence, event, lifecycle)
             |
             | bounded UDS + peer credentials
             v
       cyrene-sandboxd
       privileged Core service
             |
             | spawn, cgroup ownership, pidfd/reap, BPF device policy
             v
       worker process tree

Node/Runtime agents -- typed control session --> Kernel Core
Workspace client -- authenticated mTLS relay --> Workspace relay service
```

The compile graph and runtime graph are intentionally separate. `sandboxd` is
not made a plugin merely by using UDS; its privileged runtime responsibility
keeps it inside Platform Core.

## 6. Local finalized licensing map

The architecture classes below are now reflected by local package metadata and
the repository-level `LICENSES/` and `LICENSING.md` layout. Legal review is
still separate from this technical mapping.

| Path / Crate | Architecture Role | Proposed License Class | Reason |
| --- | --- | --- | --- |
| `runtime/cyrene-kernel` | Kernel composition root | `AGPL_CORE` | Owns the Core process and authority composition. |
| `kernel/crates/cy-kernel-daemon` | Kernel authority/host | `AGPL_CORE` | Implements lifecycle, authority, and worker orchestration. |
| `kernel/crates/cy-kernel-api` | Internal Kernel ports | `AGPL_CORE` | Internal lease/journal/sandbox/runtime ports; its public compatibility surface is not a supported third-party API. |
| `kernel/crates/cy-resource-manager` | Resource/lease authority | `AGPL_CORE` | Owns allocation and correctness semantics. |
| `kernel/crates/cy-adapter-client` | Kernel-side adapter client | `AGPL_CORE` | Core routing/provenance client despite consuming a public contract. |
| `kernel/crates/cy-sandbox-client` | Kernel-side privileged-service client | `AGPL_CORE` | Core-owned sandbox lifecycle seam. |
| `framework/crates/cy-execution-control` | Core control/session | `AGPL_CORE` | Generic execution authority and fencing integration. |
| `framework/crates/cy-installation-resolver` | Verified installation handoff | `AGPL_CORE` | Platform-owned installation/runtime trust boundary. |
| `framework/crates/cy-platform-api` | Registry/resolver implementation | `AGPL_CORE` | Internal implementation; `api` in the name is not a public-license rule. |
| `framework/crates/cy-package-runtime` | Package lifecycle host | `AGPL_CORE` | Installation, activation, and supervision implementation. |
| `agents/node/cy-node-agent` | Platform node agent | `AGPL_CORE` | Official platform process, not an extension SDK. |
| `agents/runtime/cy-runtime-agent` | Platform runtime agent | `AGPL_CORE` | Official platform lifecycle/reporting process. |
| `adapters/execution/sandboxd` | Privileged enforcement service | `AGPL_CORE` | cgroups/BPF/process cleanup authority; out-of-process is not plugin status. |
| `contracts/rust/cy-kernel-contract` | Semantic + adapter public contract | `APACHE_PUBLIC_INTERFACE` | No Core implementation dependency; independent facts, DTOs, and ports. |
| `contracts/rust/cy-proto` | Public wire contract | `APACHE_PUBLIC_INTERFACE` | Versioned generated bindings and package-local inputs. |
| `contracts/rust/cy-manifest` | Public generic contract | `APACHE_PUBLIC_INTERFACE` | Pure generic manifest/identity model. |
| `sdk/rust/cy-artifact-transfer` | Public SDK | `APACHE_PUBLIC_INTERFACE` | Public Artifact transport SDK; only public contract dependencies. |
| `framework/crates/cy-execution-fabric` | Public SDK/framework interface | `APACHE_PUBLIC_INTERFACE` | Public-only normal closure after dev-only test dependency exclusion. |
| `framework/crates/cy-workspace-fabric` | Public/client SDK | `APACHE_PUBLIC_INTERFACE` | Public wire client with primitive route config and no Core implementation edge. |
| `sdk/python/cyrene_artifacts` | Public Python SDK | `APACHE_PUBLIC_INTERFACE` | Provider-neutral SDK surface. |
| `sdk/python/cyrene_preflight` | Public Python contract/SDK | `APACHE_PUBLIC_INTERFACE` | Provider-neutral preflight surface. |
| `adapters/hardware/linux_sys` | Reference hardware adapter | `SEPARATE_DECISION` | Public protocol-compatible implementation; author/release choice remains separate. |
| `adapters/hardware/nvidia` | Reference hardware adapter | `SEPARATE_DECISION` | Same boundary; vendor-specific implementation remains replaceable. |
| Root `LICENSE`, `LICENSES/`, and `LICENSING.md` | Repository license map | `SEPARATE_DECISION` | Repository-level pointers and standard texts describe package metadata; legal notices and future relicensing policy remain separately governed. |

No current component remains `REQUIRES_SPLIT`: the former `cy-kernel-api`
ambiguity was resolved by moving shared adapter facts into the existing public
contract crate and retaining only internal ports in the Core crate. Future
public DTO growth in `cy-platform-api` or `cy-package-runtime` would require a
new split rather than silently expanding their supported API.

## 7. Extension matrix

“Can be proprietary?” is an architecture judgment, not legal advice.

| Extension | Process model | Transport | Compile dependency | Required SDK | Can be proprietary? | Architecture reason |
| --- | --- | --- | --- | --- | --- | --- |
| Worker | Separate child process under sandboxd | Worker control IPC/UDS, versioned payload | `cy-proto` and optional public SDK | `cy-proto` | Yes, by target architecture | Product/worker behavior stays outside Core. |
| Service plugin | Separate supervised process | Versioned service/control protocol | Public protocol/SDK only | `cy-proto`, public SDK as needed | Yes | `cy-package-runtime`/supervisor hosts lifecycle; service logic is replaceable. |
| Provider | Separate process | Provider/hardware protocol over UDS | `cy-kernel-contract` + `cy-proto` | Public contract | Yes | Kernel consumes generic facts and provenance, not provider implementation. |
| Hardware adapter | Separate sidecar | Hardware adapter v1 UDS | `cy-kernel-contract` + `cy-proto` | Public adapter facts and wire contract | Yes | Official NVIDIA/Linux code is reference implementation, not required linkage. |
| Sandbox | Separate privileged Core service | Sandbox v1 UDS | Core crates | Internal host API + `cy-proto` | Separate Core licensing decision | It enforces cgroups/BPF/process cleanup and is not an ecosystem plugin. |
| Node agent | Official Platform process | Typed node control protocol | Core/public contract as current graph requires | `cy-proto` | Not a supported arbitrary-license extension point | Owns platform session/reporting behavior. |
| Runtime agent | Official Platform process | Typed runtime control protocol | Core/public contract as current graph requires | `cy-proto` plus core runtime pieces | Not a supported arbitrary-license extension point | Maintains runtime lifecycle/reporting boundary. |
| Future integration | Independent process preferred | Documented gRPC/UDS/IPC | Public contract/SDK only | Contract selected by capability | Yes, if it stays on the public process seam | Prevents implementation-representation sharing. |

## 8. Package readiness

| Package | Result | Evidence |
| --- | --- | --- |
| `cy-kernel-contract` | `PASS` | `cargo package --locked` and verifier succeeded. |
| `cy-manifest` | `PASS` | `cargo package --locked` and verifier succeeded. |
| `cy-proto` | `PASS` | `cargo package --locked` succeeded with 15 proto files and local fixture included; packaged crate tests also passed 6/6. |
| `cy-artifact-transfer` | `PASS` | `cargo package --locked` verifier succeeded with temporary checked-out public dependency patches. |
| `cy-execution-fabric` | `PASS` | Same package verifier; public dependency versions are present. |
| `cy-workspace-fabric` | `PASS` | Same package verifier; `cy-execution-fabric` implementation dependency is absent. |
| `cargo publish --dry-run` | `PASS` for `cy-kernel-contract`, `cy-manifest`, and `cy-proto`; dependent SDK dry-runs are `FAIL` before first publication | Dependent dry-runs fail with the expected crates.io “no matching package” result until prerequisite public versions are published and their registry index entries propagate. No upload occurred. |

`tooling/ci/check-public-packages.sh` performs the package verification with
temporary `[patch.crates-io]` mappings to checked-out public crates. This
proves package contents and compilation before first publication while keeping
version requirements in the package manifests. It does not claim that an
unpublished crate is already available on crates.io.

## 9. Independent consumer verification

`tooling/acceptance/licensing-boundary` is its own Cargo workspace with its own
lockfile and no inherited root dependencies:

| Consumer | Direct dependency | Result |
| --- | --- | --- |
| `worker-consumer` | `cy-proto` + `prost` | `cargo check --locked`: `PASS`; encodes a versioned `WorkerToKernel` hello. |
| `hardware-consumer` | `cy-kernel-contract` | `cargo check --locked`: `PASS`; implements `HostInventoryProvider` and `ResourceProvider`. |

`tooling/acceptance/licensing-boundary/run.sh` also runs
`cargo tree --locked --edges normal,build` and rejects every Core implementation
package. Therefore the checked consumer evidence is:

```text
external consumer dependency closure ∩ Platform Core crates = empty
```

The compile-time boundary gate additionally proves:

```text
normal/build transitive closure(public crates) ∩ core_copyleft = empty
```

## 10. CI architecture/licensing gate

The new `ci / licensing-boundary` job in `.github/workflows/ci.yml` runs:

1. `tooling/ci/check-license-boundary.py` using full Cargo metadata and the
   classification source of truth;
2. `tooling/ci/check-public-proto-sync.sh` for all 15 proto files and the
   package fixture mirror;
3. `tooling/ci/check-public-packages.sh`;
4. the independent consumer workspace.

The gate rejects direct or transitive normal/build public-to-Core dependencies,
checks that all workspace packages are classified, rejects Core crate names in
public Rust sources, and makes future dependency changes fail in CI. Dev-only
tests may use Core fixtures, but those edges do not enter the published normal
closure.

## 11. Security readiness

Current sandboxd source evidence confirms:

* cgroups v2 resource controls and `cgroup.kill` cleanup;
* CPU/memory/PID/IO-related accounting/control where configured;
* cgroup-device BPF policy for hard device enforcement;
* pidfd tracking where available, bounded wait/reap, and parent-death cleanup;
* bounded UDS frames and peer UID/GID checks for sandboxd/NVIDIA, with the
  Linux system adapter's optional peer flags explicitly documented.

Current source evidence does **not** show user, mount, network, or PID
namespace isolation, seccomp, capability dropping, complete filesystem
isolation, or syscall containment. The repository now contains:

* [`SECURITY.md`](../../SECURITY.md);
* [`docs/security/threat-model.md`](../security/threat-model.md);
* updated [`docs/operations/kernel-runtime.md`](../operations/kernel-runtime.md).

The accurate claim is resource/lifecycle governance, not hostile arbitrary-code
containment or “zero orphan processes under all conditions”. The Linux system
adapter's optional UID/GID policy is a P2 hardening follow-up; this task did not
silently change its authentication model.

## 12. Clean-machine readiness

`READY_WITH_DOCUMENTATION`.

The documented CPU-only path is:

```bash
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
```

The repository's locked Python environment was also validated with
`uv run --locked python tooling/ci/verify.py --scope all-light`: governance,
documentation, SDK, and tooling checks passed (20 tests). Running the same
Python tests with the bare system interpreter was initially
`BLOCKED_BY_ENVIRONMENT` because `pytest` was not installed; that interpreter
state is not treated as code evidence.

The full local validation path additionally documents the boundary gates. A
GPU is not required for the normal workspace build/test. Production sandbox
execution requires Linux cgroup v2 delegation, protected service sockets, and
configured peer identities; NVIDIA operation additionally requires NVIDIA
tools/devices. No zero-conf production daemon or full hello-worker lifecycle
was invented for this cleanup.

Local Workspace Fabric wire validation was `BLOCKED_BY_ENVIRONMENT` because
`buf` is not installed in this task environment. The CI workflow installs Buf;
this local limitation is not reported as a protocol PASS.

## 13. Remaining blockers

| Priority | Item | Status/disposition |
| --- | --- | --- |
| P0 | Public/Core dependency firewall | Closed locally and enforced in CI; no current public-to-Core normal/build edge. |
| P1 | Formal license transition and exception/notice policy | Architecture decision finalized locally: Core metadata is `AGPL-3.0-only`, public metadata is `Apache-2.0`, and no custom plugin exception was created. Legal review and any future notice policy remain external decisions. |
| P1 | Registry release order | Explicit DAG is now documented. Package verification passes with temporary checked-out patches; dependent `cargo publish --dry-run` commands correctly fail until prerequisite versions are visible in the registry index. |
| P1 | Hostile workload isolation | Not implemented by current sandboxd. If required as a product promise, namespaces/seccomp/capability/filesystem/network isolation needs a separate security architecture project. |
| P2 | Linux system adapter default peer authentication | Optional UID/GID flags remain a deliberate compatibility/deployment difference; production should configure them explicitly or a separate hardening change should make startup fail closed. |
| P2 | Full hello-worker lifecycle example | Not present; independent compile consumers prove the firewall but not a real lease/fence/spawn/release demo. |
| P2 | Full Buf/workspace wire check in this environment | Local `buf` missing; CI setup is present. |
| P3 | Deeper rustdoc/public-API diff gate | Dependency/source gate is reliable for the current supported surface; a rustdoc JSON/public-api gate could be added later if public Rust API growth becomes material. |

## 14. Final verdict

`READY_FOR_LICENSE_FINALIZATION`

This verdict means the technical foundation for the target shape is now
present:

```text
strongly protected Platform Core
        +
public permissive contracts / SDKs
        +
arbitrary-license external process extensions
        +
machine-verifiable dependency firewall
```

It does not mean a lawyer has approved the license choice, that hosted CI has
run for this local change set, or that sandboxd is a hostile-code containment
mechanism.
