# Open-Source Architecture Readiness

本报告记录候选整改阶段的架构事实与可验证性。它不是法律意见；正式
许可证布局现已在 canonical `develop` 中由 `LICENSES/`、`LICENSING.md`
和 package metadata 表达。

## 1. Baseline

| Item | Evidence |
| --- | --- |
| Branch | `develop` in the canonical checkout; current live HEAD is the pushed documentation follow-up commit. |
| HEAD before current adapter promotion | `7b279d237161d8a6181c80d78b780c61af0fd548` |
| HEAD after current adapter promotion | `8a6a6c0bde136f474ffb15b39dba2619db9157bf`; historical adapter-boundary checkpoint. |
| Current HEAD | `bc3327bdbe1edba6449a0f34d6954b022e4c3dd0`; bilingual System/Sandbox documentation commit, read back from `origin/develop`. |
| Task worktree | The prior isolated adapter worktree is historical; the current documentation follow-up was committed on canonical `develop`. |
| Canonical checkout | `/home/baijin/Dev/Cyrene/Cyrene-Platform`; clean on `develop` and aligned with `origin/develop`. |
| PR #41 | Merge commit `dc37791a806bd6c405932f0f0457518db6e6aaa0` remains an ancestor of the current HEAD. |
| Worktree after cleanup | Intended architecture, adapter, documentation, and systemd changes are committed on `develop`; no unrelated canonical changes were taken over. |

The previous licensing-audit baseline `f56a7f30c31a729a7d4c030813356018a7be3dc1`
and adapter-boundary checkpoint `8a6a6c0` are historical evidence only. The current
live state is `develop@bc3327b`; PR #41
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
---

<!-- Chinese Translation / 中文翻译 -->

# 开源架构就绪度

本报告记录候选整改阶段的架构事实与可验证性。它不是法律意见；正式许可证布局现已在 canonical develop 中由 LICENSES/、LICENSING.md 和 package metadata 表达。

## 1. 基线

| 项目 | 证据 |
|---|---|
| 分支 | canonical checkout 中的 develop；当前 live HEAD 是已推送的文档后续提交。 |
| 本次 adapter promotion 前的 HEAD | 7b279d237161d8a6181c80d78b780c61af0fd548 |
| 本次 adapter promotion 后的 HEAD | 8a6a6c0bde136f474ffb15b39dba2619db9157bf；历史 adapter boundary checkpoint。 |
| 当前 HEAD | bc3327bdbe1edba6449a0f34d6954b022e4c3dd0；System/Sandbox 双语文档提交，已从 origin/develop 读回。 |
| 任务 worktree | 先前隔离的 adapter worktree 属于历史状态；当前文档后续工作在 canonical develop 提交。 |
| Canonical checkout | /home/baijin/Dev/Cyrene/Cyrene-Platform；develop 上干净并与 origin/develop 对齐。 |
| PR #41 | Merge commit dc37791a806bd6c405932f0f0457518db6e6aaa0 仍是当前 HEAD 的祖先。 |
| 清理后的 worktree | 预期的 architecture、adapter、documentation 和 systemd 改动已提交到 develop；没有接管无关的 canonical 变更。 |

此前 licensing audit 基线 f56a7f30c31a729a7d4c030813356018a7be3dc1 和 adapter-boundary checkpoint 8a6a6c0 仅作为历史证据。当前 live 状态是 develop@bc3327b；PR #41 也只是历史 lineage 证据，其 hosted check rollup 不视为当前绿色验收结果。

## 2. 真实 inventory 与分类

仓库当前事实：Rust workspace 有 21 个 package，2 个 Python SDK package，contracts/proto 下有 15 个规范 Protobuf 文件。旧的“16 个 core crate”说法已过时。PR #41 之后，旧 cy-plugin-sdk 和 cy-plugin-protocol 路径仍不存在。未找到既有的 examples/hello-worker 或等效的完整 lease-to-cleanup demo；新的独立 consumer 是编译边界测试，并非伪造的 lifecycle demo。

| 组件 | 架构分类 | 边界处理 |
|---|---|---|
| contracts/rust/cy-kernel-contract | PUBLIC_CONTRACT | 公共 semantic vocabulary，以及不含实现的 adapter facts/ports。 |
| contracts/rust/cy-proto | PUBLIC_CONTRACT | 有版本的 wire contract 和生成的 binding。 |
| contracts/rust/cy-manifest | PUBLIC_CONTRACT | 通用 manifest 与 identity contract。 |
| sdk/rust/cy-artifact-transfer | PUBLIC_SDK | Provider-neutral Artifact transfer SDK。 |
| framework/crates/cy-execution-fabric | PUBLIC_SDK | 使用公共 contract 的 Product-neutral admission/attachment/reconciliation API。 |
| framework/crates/cy-workspace-fabric | PUBLIC_SDK | 面向 public/client 的 relay API；不再导入 execution-fabric 实现类型。 |
| sdk/python/cyrene_artifacts | PUBLIC_SDK | Provider-neutral Python Artifact interface。 |
| sdk/python/cyrene_preflight | PUBLIC_SDK | 公共 resource-fact/preflight interface。 |
| cyrene-linux-sys-adapter、cyrene-nvidia-adapter | OUT_OF_PROCESS_ADAPTER / SEPARATE_DECISION | 参考实现；只依赖公共 cy-kernel-contract 和 cy-proto 编译。 |
| cyrene-sandboxd | PRIVILEGED_PLATFORM_SERVICE / CORE_IMPLEMENTATION | 虽在进程外运行，仍属 Core：负责 cgroups、BPF device policy、process ownership、pidfd/reaping 和 cleanup。 |
| cyrene-kernel | CORE_IMPLEMENTATION | Kernel composition root 与 authority runtime。 |
| cy-kernel-daemon、cy-resource-manager、cy-kernel-api | CORE_IMPLEMENTATION / INTERNAL_HOST_API | Lease/fence authority、resource accounting、内部 port 与 orchestration。 |
| cy-adapter-client、cy-sandbox-client | INTERNAL_LIBRARY / INTERNAL_HOST_API | Kernel 侧的有界 UDS client，不是第三方 SDK。 |
| cy-execution-control、cy-installation-resolver | CORE_IMPLEMENTATION | Core control/session 与 verified-installation handoff。 |
| cy-platform-api | INTERNAL_LIBRARY | Registry、resolver 与 repository-manifest normalization；名称中有 api 不代表它是 public。 |
| cy-package-runtime | CORE_IMPLEMENTATION / INTERNAL_HOST_API | Package lifecycle、activation 和 supervisor 实现。 |
| cy-node-agent、cy-runtime-agent | CORE_IMPLEMENTATION | 使用 typed control protocol 的 Platform Agent；二者都不是第二个 authority。 |
| tooling/architecture、tooling/ci、tooling/acceptance/licensing-boundary | TOOLING / TEST_ONLY | 可由机器验证的 boundary、package、mirror 与 external-consumer gate。 |
| Worker/provider/service plugin | OUT_OF_PROCESS_PLUGIN | 不是当前 workspace crate；通过有版本的 wire contract 和 public SDK 支持。 |

## 3. 重新验证先前发现

| 先前发现 | 当前事实 | 处理结果 |
|---|---|---|
| Workspace 有 21 个 Rust package、2 个 Python package、15 个 proto 文件 | 当前 HEAD 仍符合；旧 16-crate 描述已过时 | 从 live metadata 报告，不沿用旧报告计数。 |
| PR #41 移除了旧 plugin 路径 | 仍然如此；merge commit 是祖先，旧 crate 不存在 | 没有恢复 legacy path。 |
| cy-kernel-api 混合了 public fact 与内部 port | 基线时确认；清理后共享 adapter fact 由 cy-kernel-contract 唯一持有。cy-kernel-api 仍是面向 Core 实现的 crate，带有 public compatibility surface；只有部分 export 标为 doc(hidden)，且仍可调用 | 内部 port 和 compatibility export 仍属于 Core；pub 或 doc(hidden) 都不是受支持的第三方 extension API。 |
| 不存在 cy-kernel-api → cy-resource-manager 反向依赖 | 仍然如此；当前方向是 cy-resource-manager → cy-kernel-api | 未引入不必要的反向依赖。 |
| 独立 consumer 可在不依赖 Core 时使用 contract/protocol | 使用两个 detached Cargo consumer 重新验证并加强 | Worker 和 Hardware consumer 均可编译，其 normal/build dependency tree 不含 Core crate。 |
| cy-platform-api 因名称看起来像 public | 当前代码仍包含 registry/resolver/normalization logic | 已归类并记录为内部组件；没有强行拆分。 |
| cy-workspace-fabric::RelayClientConfig 泄漏 ConnectivityRoute | 已确认并修复 | 配置现在持有 endpoint/server-name/TLS primitive；normal dependency 不再包含 cy-execution-fabric。 |
| cy-package-runtime 混合 control format 与 host lifecycle | 当前实现仍是 lifecycle/supervisor host logic | 保持内部实现；没有宣称它是未受支持的 public SDK。 |
| SandboxBackend、Worker transport 和 raw invoke 看似 plugin API | 当前 SandboxBackend/Worker transport 仍是 Rust host internals；InstanceActor::invoke_raw 已明确标为内部 hook | 不做不必要的 RPC 重写；外部扩展指向 wire protocol。 |
| 官方 hardware adapter 导入 cy-kernel-api | 清理前确认过；现在仅导入公共 cy-kernel-contract fact 和 cy-proto | 第三方 adapter 编译依赖闭包不再要求 Kernel 实现。 |
| sandboxd 在进程外运行 | 仍然如此，但它拥有特权 enforcement 和 lifecycle | 明确归类为 PRIVILEGED_PLATFORM_SERVICE，而不是任意许可证 plugin。 |
| cy-proto package 缺少外部 proto input | 已复现并修复 | 检查 package-local proto mirror 与 core fixture mirror 是否对应 canonical source。 |
| Artifact SDK path dependency 缺少版本 | 对其公共 path dependency 已复现并修复 | 使用带版本的 path dependency，并通过 package verification patch 验证。 |
| README 缺少 clean-machine command | 当前干净任务 worktree 的 README 已提供 build/test、gate、Linux/GPU/root 要求 | 仍未声称存在完整零配置 daemon demo。 |
| Sandbox 是完整的恶意代码安全 sandbox | 当前 source 仍缺少 namespace、seccomp 和 capability dropping | Security 文档现在说明资源/lifecycle 保证及不保证事项。 |

## 4. 最终编译依赖图

此图仅表示编译期依赖。箭头代表 normal/build dependency；dev-only test dependency 不计入 public package closure gate。

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

机器可读的 source of truth 是 tooling/architecture/license-boundaries.toml。Gate 执行完整的 cargo metadata --locked --all-features，遍历 normal/build dependency edge，并拒绝任何 public 到 Core 的传递依赖路径；同时检查 public Rust source 是否引用 Core crate。

## 5. 最终运行时边界图

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

编译图与运行时图刻意分开。sandboxd 不会仅因使用 UDS 就变成 plugin；它承担特权 runtime 职责，因此仍属于 Platform Core。

## 6. 本地最终许可证映射

以下架构类别现已反映在本地 package metadata 和仓库级 LICENSES/ 与 LICENSING.md 布局中。法律审查与这份技术映射仍是分开的工作。

| 路径 / Crate | 架构角色 | 建议许可证类别 | 原因 |
|---|---|---|---|
| runtime/cyrene-kernel | Kernel composition root | AGPL_CORE | 拥有 Core process 与 authority composition。 |
| kernel/crates/cy-kernel-daemon | Kernel authority/host | AGPL_CORE | 实现 lifecycle、authority 与 worker orchestration。 |
| kernel/crates/cy-kernel-api | 内部 Kernel port | AGPL_CORE | 内部 lease/journal/sandbox/runtime port；其 public compatibility surface 不是受支持的第三方 API。 |
| kernel/crates/cy-resource-manager | Resource/lease authority | AGPL_CORE | 拥有 allocation 与正确性语义。 |
| kernel/crates/cy-adapter-client | Kernel 侧 adapter client | AGPL_CORE | 虽消费 public contract，仍负责 Core routing/provenance。 |
| kernel/crates/cy-sandbox-client | Kernel 侧特权 service client | AGPL_CORE | Core 拥有的 sandbox lifecycle seam。 |
| framework/crates/cy-execution-control | Core control/session | AGPL_CORE | 通用 execution authority 与 fencing integration。 |
| framework/crates/cy-installation-resolver | Verified installation handoff | AGPL_CORE | Platform 拥有的 installation/runtime trust boundary。 |
| framework/crates/cy-platform-api | Registry/resolver 实现 | AGPL_CORE | 内部实现；名称中的 api 不是 public license 规则。 |
| framework/crates/cy-package-runtime | Package lifecycle host | AGPL_CORE | Installation、activation 与 supervision 实现。 |
| agents/node/cy-node-agent | Platform node Agent | AGPL_CORE | 官方 Platform process，不是 extension SDK。 |
| agents/runtime/cy-runtime-agent | Platform runtime Agent | AGPL_CORE | 官方 Platform lifecycle/reporting process。 |
| adapters/execution/sandboxd | 特权 enforcement service | AGPL_CORE | 拥有 cgroups/BPF/process cleanup authority；进程外运行不等于 plugin。 |
| contracts/rust/cy-kernel-contract | Semantic + adapter public contract | APACHE_PUBLIC_INTERFACE | 不依赖 Core 实现；提供独立 fact、DTO 与 port。 |
| contracts/rust/cy-proto | Public wire contract | APACHE_PUBLIC_INTERFACE | 有版本的 generated binding 与 package-local input。 |
| contracts/rust/cy-manifest | Public generic contract | APACHE_PUBLIC_INTERFACE | 纯通用 manifest/identity model。 |
| sdk/rust/cy-artifact-transfer | Public SDK | APACHE_PUBLIC_INTERFACE | Public Artifact transport SDK；只依赖 public contract。 |
| framework/crates/cy-execution-fabric | Public SDK/framework interface | APACHE_PUBLIC_INTERFACE | 排除 dev-only test dependency 后，normal closure 仅含 public dependency。 |
| framework/crates/cy-workspace-fabric | Public/client SDK | APACHE_PUBLIC_INTERFACE | Public wire client 使用 primitive route config，不依赖 Core implementation。 |
| sdk/python/cyrene_artifacts | Public Python SDK | APACHE_PUBLIC_INTERFACE | Provider-neutral SDK surface。 |
| sdk/python/cyrene_preflight | Public Python contract/SDK | APACHE_PUBLIC_INTERFACE | Provider-neutral preflight surface。 |
| adapters/hardware/linux_sys | Reference hardware adapter | SEPARATE_DECISION | 协议兼容的 public 实现；作者/发布选择另行决策。 |
| adapters/hardware/nvidia | Reference hardware adapter | SEPARATE_DECISION | 边界相同；vendor-specific 实现可以替换。 |
| 根 LICENSE、LICENSES/ 与 LICENSING.md | 仓库许可证映射 | SEPARATE_DECISION | 仓库指针和标准文本描述 package metadata；legal notice 与未来 relicensing policy 仍由单独流程治理。 |

当前没有组件仍处于 REQUIRES_SPLIT：此前 cy-kernel-api 的歧义已通过将共享 adapter fact 移至现有 public contract crate、并仅在 Core crate 中保留内部 port 解决。若未来 cy-platform-api 或 cy-package-runtime 增加 public DTO，则需要新的 split，不能静默扩展其受支持 API。

## 7. Extension 矩阵

“能否使用专有许可证？”是架构判断，不是法律意见。

| Extension | 进程模型 | Transport | 编译依赖 | 所需 SDK | 能否使用专有许可证？ | 架构原因 |
|---|---|---|---|---|---|---|
| Worker | sandboxd 下的独立 child process | Worker control IPC/UDS，有版本的 payload | cy-proto 和可选 public SDK | cy-proto | 可以，按目标架构允许 | Product/worker 行为留在 Core 之外。 |
| Service plugin | 独立受监管进程 | 有版本的 service/control protocol | 仅 public protocol/SDK | 按需使用 cy-proto 与 public SDK | 可以 | cy-package-runtime/supervisor 承载 lifecycle；service logic 可替换。 |
| Provider | 独立进程 | 经 UDS 传输的 Provider/hardware protocol | cy-kernel-contract + cy-proto | Public contract | 可以 | Kernel 消费通用 fact 与 provenance，不消费 Provider 实现。 |
| Hardware adapter | 独立 sidecar | Hardware Adapter v1 UDS | cy-kernel-contract + cy-proto | Public adapter fact 与 wire contract | 可以 | 官方 NVIDIA/Linux code 是参考实现，不是必需链接依赖。 |
| Sandbox | 独立特权 Core service | Sandbox v1 UDS | Core crate | 内部 host API + cy-proto | 需单独决定 Core 许可 | 它负责 cgroups/BPF/process cleanup，不是生态 plugin。 |
| Node Agent | 官方 Platform process | Typed node control protocol | 按当前图使用 Core/public contract | cy-proto | 不是受支持的任意许可证 extension point | 拥有 Platform session/reporting 行为。 |
| Runtime Agent | 官方 Platform process | Typed runtime control protocol | 按当前图使用 Core/public contract | cy-proto 加 Core runtime 组件 | 不是受支持的任意许可证 extension point | 维护 runtime lifecycle/reporting boundary。 |
| Future integration | 优先采用独立进程 | 文档化的 gRPC/UDS/IPC | 仅 public contract/SDK | 按 capability 选择 contract | 若留在 public process seam，则可以 | 避免共享实现表示。 |

## 8. Package 就绪情况

| Package | 结果 | 证据 |
|---|---|---|
| cy-kernel-contract | PASS | cargo package --locked 与 verifier 成功。 |
| cy-manifest | PASS | cargo package --locked 与 verifier 成功。 |
| cy-proto | PASS | cargo package --locked 成功；包含 15 个 proto 文件和本地 fixture；打包后 crate test 也以 6/6 通过。 |
| cy-artifact-transfer | PASS | cargo package --locked verifier 使用临时的 checked-out public dependency patch 后成功。 |
| cy-execution-fabric | PASS | 同一 package verifier 成功；存在 public dependency version。 |
| cy-workspace-fabric | PASS | 同一 package verifier 成功；不存在 cy-execution-fabric implementation dependency。 |
| cargo publish --dry-run | cy-kernel-contract、cy-manifest 和 cy-proto 为 PASS；依赖其他 package 的 SDK dry-run 在首次发布前为 FAIL | 依赖 package 的 dry-run 按预期因 crates.io “no matching package” 失败，直到前置 public version 发布且 registry index 传播完成。没有上传 package。 |

tooling/ci/check-public-packages.sh 使用临时的 [patch.crates-io] mapping 指向已 checkout 的 public crate 来执行 package verification。这能在首次发布之前证明 package 内容和编译，同时保留 manifest 中的版本要求；它不代表未发布的 crate 已可从 crates.io 获取。


## 9. 独立 consumer 验证

tooling/acceptance/licensing-boundary 是拥有独立 lockfile 的独立 Cargo workspace，不继承 root dependency：

| Consumer | 直接依赖 | 结果 |
|---|---|---|
| worker-consumer | cy-proto + prost | cargo check --locked：PASS；编码有版本的 WorkerToKernel hello。 |
| hardware-consumer | cy-kernel-contract | cargo check --locked：PASS；实现 HostInventoryProvider 和 ResourceProvider。 |

tooling/acceptance/licensing-boundary/run.sh 还运行 cargo tree --locked --edges normal,build，并拒绝任何 Core implementation package。因此，已检查的 consumer 证据为：

```text
external consumer dependency closure ∩ Platform Core crates = empty
```

编译期 boundary gate 还证明：

```text
normal/build transitive closure(public crates) ∩ core_copyleft = empty
```

## 10. CI architecture/licensing gate

.github/workflows/ci.yml 中新增的 ci / licensing-boundary job 会运行：

1. 使用完整 Cargo metadata 和分类 source of truth 的 tooling/ci/check-license-boundary.py；
2. 对全部 15 个 proto 文件和 package fixture mirror 执行 tooling/ci/check-public-proto-sync.sh；
3. tooling/ci/check-public-packages.sh；
4. 独立 consumer workspace。

该 gate 拒绝 public 到 Core 的直接或传递 normal/build dependency，检查 workspace package 是否全部分类，拒绝 public Rust source 中出现 Core crate 名称，并确保未来 dependency 变更会在 CI 中失败。Dev-only test 可以使用 Core fixture，但这些 edge 不属于已发布的 normal closure。

## 11. Security 就绪情况

当前 sandboxd source 证据确认：

- cgroups v2 resource control 和 cgroup.kill cleanup；
- 在已配置时提供 CPU/memory/PID/IO 相关 accounting/control；
- 通过 cgroup-device BPF policy 实施硬件设备强制策略；
- 在可用时跟踪 pidfd，执行有界 wait/reap 和 parent-death cleanup；
- sandboxd/NVIDIA 使用有界 UDS frame 和 peer UID/GID 检查；Linux system adapter 的可选 peer flag 已明确记录。

当前 source 证据没有显示 user、mount、network 或 PID namespace isolation、seccomp、capability dropping、完整 filesystem isolation 或 syscall containment。仓库现在包含 SECURITY.md、docs/security/threat-model.md 和更新后的 docs/operations/kernel-runtime.md。

准确的能力描述应是资源/lifecycle governance，而不是恶意任意代码 containment，也不是“任何情况下都没有孤儿进程”。Linux system adapter 的可选 UID/GID policy 是 P2 hardening follow-up；本任务没有静默改变其 authentication model。

## 12. Clean-machine 就绪情况

状态：READY_WITH_DOCUMENTATION。

已记录的 CPU-only 路径为：

```bash
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
```

仓库锁定的 Python environment 也已通过 uv run --locked python tooling/ci/verify.py --scope all-light 验证：governance、documentation、SDK 和 tooling check 通过（20 项 test）。最初用裸系统 interpreter 运行相同 Python test 时，由于未安装 pytest 而被标记为 BLOCKED_BY_ENVIRONMENT；该 interpreter 状态不作为代码证据。

完整本地验证路径还记录了 boundary gate。常规 workspace build/test 不需要 GPU。生产 sandbox execution 需要 Linux cgroup v2 delegation、受保护的 service socket 和已配置 peer identity；NVIDIA operation 还需要 NVIDIA tool/device。本次清理没有虚构零配置生产 daemon 或完整的 hello-worker lifecycle。

本地 Workspace Fabric wire validation 因任务环境没有安装 buf 而被标记为 BLOCKED_BY_ENVIRONMENT。CI workflow 会安装 Buf；不会把这一项本地限制报告为 protocol PASS。

## 13. 剩余阻塞项

| 优先级 | 项目 | 状态/处理 |
|---|---|---|
| P0 | Public/Core dependency firewall | 本地已关闭并在 CI 强制执行；当前没有 public 到 Core 的 normal/build edge。 |
| P1 | 正式许可证迁移与 exception/notice policy | 本地已完成架构决策：Core metadata 为 AGPL-3.0-only，public metadata 为 Apache-2.0，且未创建自定义 plugin exception。法律审查及任何未来 notice policy 仍属外部决策。 |
| P1 | Registry release 顺序 | 已明确记录 DAG。使用临时 checked-out patch 的 package verification 通过；在前置版本出现在 registry index 前，依赖它们的 cargo publish --dry-run 按预期失败。 |
| P1 | 恶意 workload 隔离 | 当前 sandboxd 未实现。若要作为产品承诺，则需另立 security architecture 项目实现 namespace/seccomp/capability/filesystem/network isolation。 |
| P2 | Linux system adapter 默认 peer authentication | 可选 UID/GID flag 仍是有意保留的 compatibility/deployment 差异；生产环境应显式配置，或另一个 hardening 变更应让启动 fail closed。 |
| P2 | 完整 hello-worker lifecycle 示例 | 目前不存在；独立编译 consumer 证明 firewall，但不证明真实 lease/fence/spawn/release demo。 |
| P2 | 本环境中的完整 Buf/workspace wire check | 本地缺少 buf；CI setup 已具备。 |
| P3 | 更深入的 rustdoc/public-API diff gate | 对当前 supported surface，dependency/source gate 足够可靠；若未来 public Rust API 增长显著，可增加 rustdoc JSON/public-api gate。 |

## 14. 最终结论

READY_FOR_LICENSE_FINALIZATION

此结论表示目标架构形态所需的技术基础现已具备：

```text
strongly protected Platform Core
        +
public permissive contracts / SDKs
        +
arbitrary-license external process extensions
        +
machine-verifiable dependency firewall
```

它不表示律师已批准许可证选择，不表示 hosted CI 已针对这组本地变更运行，也不表示 sandboxd 是恶意代码 containment 机制。
