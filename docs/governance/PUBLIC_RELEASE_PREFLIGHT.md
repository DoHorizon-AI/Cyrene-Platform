# Cyrene-Platform Public Release Preflight

Status: `READY_TO_MAKE_PUBLIC_WITH_POST_PUBLIC_CI`

This is a technical repository preflight, not legal advice. It records the
state of the canonical `develop` checkout after promoting the licensing-boundary
cleanup, finalizing the repository license layout, and pushing the bilingual
documentation follow-up. No repository visibility change, tag, release, or
registry upload was performed.

## Baseline and delivery

| Item | Evidence |
| --- | --- |
| Canonical branch | `develop` |
| Canonical HEAD before cleanup | `f56a7f30c31a729a7d4c030813356018a7be3dc1` (historical baseline) |
| Architecture cleanup commit | `ddb342f` — `architecture: finalize public core licensing boundary` |
| License finalization commit | `06e6ac5` — `legal: finalize repository license layout` |
| Current canonical HEAD | `bc3327bdbe1edba6449a0f34d6954b022e4c3dd0` (`docs: add bilingual system and sandbox adapter guides`) |
| Remote state | `origin/develop` reads back the same `bc3327b` commit; no force push was used |
| Candidate source | Historical licensing candidate only; current canonical docs were edited and verified in the repository checkout |

The canonical user README change was preserved and semantically merged. The
final README retains the user's Platform-as-generic-substrate positioning,
Product ownership boundary, Linux/systemd guidance, architecture links, and
security caveats. Empty legacy command blocks and obsolete license wording were
replaced with the current quickstart and tiered licensing model; the candidate
README was not copied over the user's file as a whole.

## Architecture inventory

| Role | Current components | Boundary decision |
| --- | --- | --- |
| Platform Core implementation | `cyrene-kernel`, `cy-kernel-api`, `cy-kernel-daemon`, `cy-resource-manager`, `cy-execution-control`, `cy-platform-api`, `cy-package-runtime`, `cy-installation-resolver`, `cy-adapter-client`, `cy-sandbox-client`, `cy-node-agent`, `cy-runtime-agent` | AGPL Core implementation and internal host API |
| Public Contracts | `cy-kernel-contract`, `cy-proto`, `cy-manifest` | Apache-2.0 facts, schemas, protocol inputs, generated public bindings |
| Public SDK | `cy-artifact-transfer`, `cy-execution-fabric`, `cy-workspace-fabric`, `cyrene-artifacts`, `cyrene-preflight` | Apache-2.0 packages consumable outside the monorepo |
| Official reference adapters | `cyrene-linux-sys-adapter`, `cyrene-nvidia-adapter` | Separate licensing decision; current metadata is Apache-2.0 |
| Privileged Platform Service | `cyrene-sandboxd` | Core service despite being out of process; owns enforcement and cleanup |
| Internal Host API | lease, journal, sandbox, runtime, orchestration ports; `SandboxBackend`, `WorkerTransport*`, `InstanceActor::invoke_raw` paths | Core-only; not the supported third-party plugin ABI |
| Tooling and tests | boundary gates, package verifier, mirror gate, external consumers, governance checks | Repository tooling; no implementation dependency is exposed as public API |

`cy-kernel-contract` was manually checked after the cleanup. Its public traits
are `HostInventoryProvider`, `ResourceProvider`, and `SystemAdapter`, plus
their implementation-free semantic facts. It does not contain
`ResourceLeaseManager`, `JournalPort`, or
the sandbox/runtime orchestration ports. Those remain in Core. `cy-kernel-api`
retains a Core compatibility surface, including `doc(hidden)` exports;
`doc(hidden)` is not access control, but no Apache public crate reverse-
re-exports those Core types.

## Compile dependency graph

The public package graph, as verified by Cargo metadata, is:

```text
cy-kernel-contract   (leaf)
cy-proto             (leaf)
cy-manifest          (leaf)
        |                  \
        v                   v
cy-artifact-transfer   cy-execution-fabric
        |                   |
        +-------------------+  (public contracts only; artifact transfer is dev-only there)

cy-workspace-fabric -> cy-proto
```

The machine-enforced invariant is stronger than the abbreviated graph:

```text
Public normal/build dependency closure
        ∩
Core implementation packages
        = empty
```

`tooling/ci/check-license-boundary.py` validates the complete Cargo normal and
build closure, traverses transitive edges, rejects unclassified workspace
packages, and rejects public Rust source references to Core crate names. Dev
dependencies are excluded from the published normal/build closure and are
documented separately.

## Runtime boundary graph

```text
Independent Worker / Provider / Service / Hardware Adapter
                         |
                         | documented gRPC / UDS / IPC
                         v
                 Public Contracts / SDK
                         |
                         | authenticated protocol boundary
                         v
                 Platform Core authority
                         |
                         | privileged UDS / spawn / lifecycle control
                         v
                    cyrene-sandboxd
```

Process separation is not used as the licensing classification by itself.
`cyrene-sandboxd` remains Core because it performs cgroup enforcement, device
policy, process lifecycle ownership, recovery, and cleanup. Node and Runtime
Agents are Platform-owned implementations, not arbitrary-license plugins.

## Licensing map

| Path / crate | Architecture role | Proposed/finalized class | Reason |
| --- | --- | --- | --- |
| `cy-adapter-client` | Core adapter client | `AGPL_CORE` | Core-owned authenticated client and registry behavior |
| `cy-execution-control` | Core control plane | `AGPL_CORE` | Core execution authority |
| `cy-installation-resolver` | Core resolver | `AGPL_CORE` | Core installation and verification implementation |
| `cy-kernel-api` | Core API and compatibility surface | `AGPL_CORE` | Internal ports and Core compatibility exports remain here |
| `cy-kernel-daemon` | Kernel authority | `AGPL_CORE` | Lease, Fence, event, and lifecycle implementation |
| `cy-node-agent` | Platform agent | `AGPL_CORE` | Platform-owned node lifecycle implementation |
| `cy-package-runtime` | Runtime host | `AGPL_CORE` | Supervisor and host lifecycle implementation |
| `cy-platform-api` | Core registry/resolver | `AGPL_CORE` | Implementation-oriented registry and resolution logic |
| `cy-resource-manager` | Resource authority | `AGPL_CORE` | Core ownership and allocation implementation |
| `cy-runtime-agent` | Platform agent | `AGPL_CORE` | Platform-owned runtime lifecycle implementation |
| `cy-sandbox-client` | Core privileged client | `AGPL_CORE` | Core-to-sandbox enforcement client |
| `cyrene-kernel` | Composition root | `AGPL_CORE` | Core runtime composition |
| `cyrene-sandboxd` | Privileged Core service | `AGPL_CORE` | Enforcement and cleanup service |
| `cy-kernel-contract` | Public adapter contract | `APACHE_PUBLIC_INTERFACE` | Implementation-free public facts and SPI |
| `cy-proto` | Public protocol bindings | `APACHE_PUBLIC_INTERFACE` | Package-local Apache protocol inputs and bindings |
| `cy-manifest` | Public manifest contract | `APACHE_PUBLIC_INTERFACE` | Reusable schema/manifest contract |
| `cy-artifact-transfer` | Public Artifact SDK | `APACHE_PUBLIC_INTERFACE` | External package SDK |
| `cy-execution-fabric` | Public execution SDK | `APACHE_PUBLIC_INTERFACE` | Public protocol/client-facing types |
| `cy-workspace-fabric` | Public workspace SDK | `APACHE_PUBLIC_INTERFACE` | Public client-facing types without Core implementation types |
| `cyrene-linux-sys-adapter` | Official reference adapter | `SEPARATE_DECISION` | Current Apache metadata; policy kept separate from public SPI |
| `cyrene-nvidia-adapter` | Official reference adapter | `SEPARATE_DECISION` | Current Apache metadata; policy kept separate from public SPI |
| `sdk/python/cyrene_artifacts` | Public Python SDK | `APACHE_PUBLIC_INTERFACE` | Apache-2.0 package metadata |
| `sdk/python/cyrene_preflight` | Public Python SDK | `APACHE_PUBLIC_INTERFACE` | Apache-2.0 package metadata |
| `contracts/proto/` and `contracts/schemas/` | Public source contracts | `APACHE_PUBLIC_INTERFACE` | SPDX-marked protocol/schema inputs |

The repository now contains `LICENSES/AGPL-3.0-only.txt`,
`LICENSES/Apache-2.0.txt`, a root license pointer, and `LICENSING.md`. The
package metadata gate reports 13 Core, 6 public, and 2 separate-decision Rust
packages. No formal license text was created for a plugin exception.

## Extension matrix

| Extension | Process model / transport | Compile dependency | Can be proprietary?* | Reason |
| --- | --- | --- | --- | --- |
| Worker | Independent process; worker control IPC/UDS | `cy-proto`, optional public SDK | Yes | Uses documented process protocol |
| Provider | Independent process; versioned provider UDS/gRPC | `cy-kernel-contract` + `cy-proto` | Yes | Public provider facts and wire contract |
| Service plugin | Independent service; versioned service/control protocol | Public protocol/SDK only | Yes | No Core link-time ABI |
| Hardware adapter | Independent adapter process; Hardware Adapter v1 UDS | `cy-kernel-contract` + `cy-proto` | Yes, for independently developed adapters | Public adapter SPI; reference implementations are separate decisions |
| Sandbox | Privileged Platform service; Core UDS/spawn | Core/internal APIs | No arbitrary-plugin classification | Enforcement responsibility makes it Core |
| Node Agent | Platform-owned process; authenticated control UDS | Core implementation | Platform Core policy | Owns Platform node lifecycle |
| Runtime Agent | Platform-owned process; authenticated control UDS | Core implementation | Platform Core policy | Owns Platform runtime lifecycle |
| Future integration | Independent process at a documented public seam | Public contracts/SDK only | Yes | Must remain independently developed and out of the Core link closure |

\* “Can be proprietary?” is an architectural design judgment, not a legal
opinion. The intended statement applies only to independently developed
extensions using designated public process interfaces; it does not change the
license for modifications to Platform Core.

Architecture conclusion: `PLUGIN_EXCEPTION_NOT_ARCHITECTURALLY_REQUIRED`.
The current supported extension paths do not require linking an AGPL Core
implementation crate. A legal review may still choose a different distribution
policy.

## Validation results

### Architecture and packages

| Gate | Result | Evidence |
| --- | --- | --- |
| Public/Core dependency firewall | `PASS` | `python3 tooling/ci/check-license-boundary.py` |
| License metadata | `PASS` | 6 public, 13 Core, 2 separate-decision packages |
| Public Proto mirror | `PASS` | `bash tooling/ci/check-public-proto-sync.sh` |
| Independent Worker/Hardware consumers | `PASS` | `bash tooling/acceptance/licensing-boundary/run.sh`; closure contains no Core package |
| Public package contents | `PASS` | `bash tooling/ci/check-public-packages.sh`; 6/6 package and verify passes |
| Leaf `cargo publish --dry-run` | `PASS` | `cy-kernel-contract`, `cy-proto`, `cy-manifest` |
| Dependent `cargo publish --dry-run` | `DEFERRED_REGISTRY` | Expected crates.io lookup failure until prerequisite packages are published and the registry index propagates; no package upload occurred |

The release DAG and registry propagation procedure are documented in
`docs/release/PUBLIC_PACKAGE_RELEASE_ORDER.md`. The package verifier uses
temporary `[patch.crates-io]` mappings only for local package-content and
compilation verification; this is not treated as proof that the crates are
already published.

### Local build and test

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | `PASS` |
| `cargo check --locked --workspace --all-targets --all-features` | `PASS` |
| `cargo test --locked --workspace --all-features` | `PASS` |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | `PASS` |
| `uv run --locked python tooling/ci/verify.py --scope all-light` | `PASS` after this report is added; prior run was blocked only by this report's not-yet-created link |
| Architecture/governance/kernel-boundary checks | `PASS` |
| Buf distributed workspace check | `BLOCKED_BY_ENVIRONMENT` locally because `buf` is not installed; CI installs Buf through its workflow action |

The full Rust test run includes expected ignored tests requiring root or a
second local account. No GPU or privileged test result is being represented as
passed merely because compilation succeeded.

### Security readiness

`SECURITY.md` and `docs/security/threat-model.md` document the trust,
authentication, privilege, and enforcement boundaries. The current sandbox
implementation provides cgroup v2 resource controls, CPU/memory/PID/IO
accounting or limits, BPF device policy, pidfd-based tracking where available,
and cgroup-scoped cleanup.

It does not provide user/mount/network/PID namespace isolation, seccomp,
capability dropping, complete host-filesystem isolation, or syscall
containment. It must not be marketed as hostile arbitrary-code containment or
as a complete multi-tenant security sandbox. The documented claim is bounded
cgroup-scoped lifecycle management and cleanup.

### Current-tree and history sensitive-data audit

| Audit | Result | Scope and limitations |
| --- | --- | --- |
| Current tree high-confidence scan | `PASS` | Tracked and non-ignored files; private-key, common cloud/token formats, credential assignments, and sensitive filename patterns; no secret values printed |
| Reachable history high-confidence scan | `PASS` | 6,978 reachable Git objects across `--all` refs; no high-confidence credential patterns |
| Historical filename review | `PASS` | One old `infrastructure/cyrene-runtime-deployment/.env.example`, classified `OK_TO_PUBLISH_HISTORY` as example configuration, not credential material |
| Private endpoint check | `PASS` | No private-network literals; two localhost URLs are test/acceptance endpoints |
| Mature secret scanners | `NOT_INSTALLED` | `gitleaks` and `trufflehog` are unavailable locally; no claim of scanner-equivalent coverage is made |

The intended public refs currently include local/remote `develop` and `main`
plus the remote HEAD symbolic ref; no tags are present in the local ref set.
No destructive history rewrite was performed. If a future stronger scanner
finds a real historical credential, it must be rotated and history rewrite
must be separately authorized and backed up.

### Third-party license audit

`cargo metadata --locked --all-features` reports 280 external Cargo packages
with zero missing `license`/`license_file` metadata. The locked Python set has
19 packages; 17 installed distributions expose `License`,
`License-Expression`, or `License-File` metadata and the two not installed are
the local workspace package and a conditional package. No tracked vendored or
third-party source directory was found. `cargo-deny` is not installed, so this
is a metadata/provenance preflight rather than a legal dependency opinion.

Result: `PASS_WITH_TOOLING_LIMITATION`; no repository-level conflict or copied
source finding was identified by the available checks.

## Clean-machine and hosted CI readiness

| Area | Result | Interpretation |
| --- | --- | --- |
| README build/test instructions | `READY_WITH_DOCUMENTATION` | CPU-only build/test path, Linux/cgroup/NVIDIA prerequisites, and pre-release runtime status are explicit |
| Fresh Ubuntu clone/build/test/lifecycle | `DEFERRED_UNTIL_PUBLIC` | No fresh GitHub-hosted Ubuntu runner was executed in this private repository context |
| Hosted GitHub CI | `DEFERRED_UNTIL_PUBLIC` | Not manually triggered or monitored in the local delivery; repository push workflows may auto-enqueue after a normal push |
| Azure pipeline | `NOT_TRIGGERED` | Repository configuration has `trigger: none` and `pr: none` |

The remaining hosted gate should use an uncached `ubuntu-24.04` environment,
install documented prerequisites including Buf, build and test the workspace,
run the minimal non-GPU worker path, and shut down all services. Production
sandbox mode must retain its real privilege and cgroup requirements.

## Remaining items by priority

### P0

None identified.

### P1

- Run hosted GitHub CI and the fresh Ubuntu lifecycle gate after the repository
  is made public. This is intentionally post-public evidence, not a hidden
  local PASS.

### P2

- Install/run Buf in the hosted wire-check environment; local absence is an
  environment limitation.
- Run a policy-grade third-party license scanner when the release environment
  provides one; the available Cargo/Python metadata checks passed.
- If dual licensing or broad relicensing is later pursued, obtain legal advice
  and adopt an explicit CLA/DCO policy. The repository currently does not
  fabricate one.

### P3

- Keep the public package registry release order and propagation wait in the
  release checklist.
- Perform the real privileged/root/cgroup acceptance matrix in the deployment
  environment before claiming production sandbox readiness.

## Final verdict

`READY_TO_MAKE_PUBLIC_WITH_POST_PUBLIC_CI`

The technical boundary is now present in source layout, Cargo dependency
metadata, CI gates, package verification, independent consumers, and runtime
documentation. The repository can be made public without changing visibility
or publishing artifacts in this task. Hosted CI, fresh-machine lifecycle
evidence, and the Buf environment check remain required after public exposure
and before a production release claim.
---

<!-- Chinese Translation / 中文翻译 -->

# Cyrene-Platform 公开发布预检

状态：READY_TO_MAKE_PUBLIC_WITH_POST_PUBLIC_CI

这是技术性仓库预检，不是法律意见。它记录了 canonical develop checkout 在推广 licensing-boundary cleanup、确定仓库许可证布局并推送双语文档后续内容之后的状态。本次未更改仓库可见性、未打 tag、未发布 release，也未上传 registry package。

## 基线与交付

| 项目 | 证据 |
|---|---|
| Canonical branch | develop |
| 清理前的 canonical HEAD | f56a7f30c31a729a7d4c030813356018a7be3dc1（历史基线） |
| Architecture cleanup commit | ddb342f — architecture: finalize public core licensing boundary |
| License finalization commit | 06e6ac5 — legal: finalize repository license layout |
| 当前 canonical HEAD | bc3327bdbe1edba6449a0f34d6954b022e4c3dd0（docs: add bilingual system and sandbox adapter guides） |
| Remote 状态 | origin/develop 读回相同的 bc3327b commit；未使用 force push |
| Candidate source | 仅为历史 licensing candidate；当前 canonical 文档已在仓库 checkout 中编辑并验证 |

用户的 canonical README 改动得到保留并按语义合并。最终 README 保留了用户将 Platform 定位为通用 substrate、Product ownership boundary、Linux/systemd 指南、architecture 链接和 security caveat 的表达。空的 legacy command block 和过期的许可证措辞被当前 quickstart 与分级 licensing model 替代；没有把 candidate README 整体覆盖到用户文件上。

## 架构 inventory

| 角色 | 当前组件 | 边界决策 |
|---|---|---|
| Platform Core 实现 | cyrene-kernel、cy-kernel-api、cy-kernel-daemon、cy-resource-manager、cy-execution-control、cy-platform-api、cy-package-runtime、cy-installation-resolver、cy-adapter-client、cy-sandbox-client、cy-node-agent、cy-runtime-agent | AGPL Core implementation 与内部 host API |
| Public Contract | cy-kernel-contract、cy-proto、cy-manifest | Apache-2.0 facts、schema、protocol input 与 generated public binding |
| Public SDK | cy-artifact-transfer、cy-execution-fabric、cy-workspace-fabric、cyrene-artifacts、cyrene-preflight | 可在 monorepo 外消费的 Apache-2.0 package |
| 官方参考 adapter | cyrene-linux-sys-adapter、cyrene-nvidia-adapter | 许可证另行决策；当前 metadata 为 Apache-2.0 |
| 特权 Platform Service | cyrene-sandboxd | 即使运行于进程外仍属 Core；拥有 enforcement 与 cleanup |
| 内部 Host API | lease、journal、sandbox、runtime、orchestration port；SandboxBackend、WorkerTransport*、InstanceActor::invoke_raw 路径 | 仅 Core 使用；不是受支持的第三方 plugin ABI |
| Tooling 和 test | boundary gate、package verifier、mirror gate、external consumer、governance check | 仓库 tooling；未将实现依赖作为 public API 暴露 |

cleanup 后人工检查了 cy-kernel-contract。其 public trait 包括 HostInventoryProvider、ResourceProvider 和 SystemAdapter，以及不包含实现的 semantic fact。它不含 ResourceLeaseManager、JournalPort，也不含 sandbox/runtime orchestration port；这些仍位于 Core。cy-kernel-api 保留 Core compatibility surface，包括 doc(hidden) export；doc(hidden) 不是访问控制，但没有 Apache public crate 反向 re-export 这些 Core type。

## 编译依赖图

根据 Cargo metadata 验证的 public package graph 如下：

```text
cy-kernel-contract   (leaf)
cy-proto             (leaf)
cy-manifest          (leaf)
        |                  \
        v                   v
cy-artifact-transfer   cy-execution-fabric
        |                   |
        +-------------------+  (only public contracts; artifact transfer is dev-only there)

cy-workspace-fabric -> cy-proto
```

机器强制的不变量比简化图更严格：

```text
Public normal/build dependency closure
        ∩
Core implementation packages
        = empty
```

tooling/ci/check-license-boundary.py 验证完整 Cargo normal/build closure，遍历传递依赖，拒绝未分类的 workspace package，并拒绝 public Rust source 引用 Core crate 名称。Dev dependency 不进入发布后的 normal/build closure，并单独记录。

## 运行时边界图

```text
Independent Worker / Provider / Service / Hardware Adapter
                         |
                         | documented gRPC / UDS / IPC
                         v
                 Public Contracts / SDK
                         |
                         | authenticated protocol boundary
                         v
                 Platform Core authority
                         |
                         | privileged UDS / spawn / lifecycle control
                         v
                    cyrene-sandboxd
```

许可证分类不能只依据进程是否分离。cyrene-sandboxd 执行 cgroup enforcement、device policy、process lifecycle ownership、recovery 和 cleanup，因此仍属于 Core。Node 与 Runtime Agent 是 Platform 所有的实现，不是任意许可证 plugin。

## 许可证映射

| Path / crate | 架构角色 | 建议/最终类别 | 原因 |
|---|---|---|---|
| cy-adapter-client | Core adapter client | AGPL_CORE | Core 所有的认证 client 和 registry 行为 |
| cy-execution-control | Core control plane | AGPL_CORE | Core execution authority |
| cy-installation-resolver | Core resolver | AGPL_CORE | Core installation 与 verification 实现 |
| cy-kernel-api | Core API 与 compatibility surface | AGPL_CORE | 内部 port 与 Core compatibility export 保留在此 |
| cy-kernel-daemon | Kernel authority | AGPL_CORE | Lease、Fence、Event 和 lifecycle 实现 |
| cy-node-agent | Platform Agent | AGPL_CORE | Platform 所有的 node lifecycle 实现 |
| cy-package-runtime | Runtime host | AGPL_CORE | Supervisor 与 host lifecycle 实现 |
| cy-platform-api | Core registry/resolver | AGPL_CORE | 面向实现的 registry 和 resolution logic |
| cy-resource-manager | Resource authority | AGPL_CORE | Core ownership 与 allocation 实现 |
| cy-runtime-agent | Platform Agent | AGPL_CORE | Platform 所有的 runtime lifecycle 实现 |
| cy-sandbox-client | Core 特权 client | AGPL_CORE | Core 到 sandbox 的 enforcement client |
| cyrene-kernel | Composition root | AGPL_CORE | Core runtime composition |
| cyrene-sandboxd | 特权 Core service | AGPL_CORE | Enforcement 与 cleanup service |
| cy-kernel-contract | Public adapter contract | APACHE_PUBLIC_INTERFACE | 不含实现的 public fact 与 SPI |
| cy-proto | Public protocol binding | APACHE_PUBLIC_INTERFACE | Package-local Apache protocol input 与 binding |
| cy-manifest | Public manifest contract | APACHE_PUBLIC_INTERFACE | 可复用 schema/manifest contract |
| cy-artifact-transfer | Public Artifact SDK | APACHE_PUBLIC_INTERFACE | 外部 package SDK |
| cy-execution-fabric | Public execution SDK | APACHE_PUBLIC_INTERFACE | 面向 public protocol/client 的 type |
| cy-workspace-fabric | Public workspace SDK | APACHE_PUBLIC_INTERFACE | 不含 Core implementation type 的面向 client type |
| cyrene-linux-sys-adapter | 官方参考 adapter | SEPARATE_DECISION | 当前 Apache metadata；policy 与 public SPI 分开 |
| cyrene-nvidia-adapter | 官方参考 adapter | SEPARATE_DECISION | 当前 Apache metadata；policy 与 public SPI 分开 |
| sdk/python/cyrene_artifacts | Public Python SDK | APACHE_PUBLIC_INTERFACE | Apache-2.0 package metadata |
| sdk/python/cyrene_preflight | Public Python SDK | APACHE_PUBLIC_INTERFACE | Apache-2.0 package metadata |
| contracts/proto/ 与 contracts/schemas/ | Public source contract | APACHE_PUBLIC_INTERFACE | 标有 SPDX 的 protocol/schema input |

仓库现在包含 LICENSES/AGPL-3.0-only.txt、LICENSES/Apache-2.0.txt、根许可证指针和 LICENSING.md。Package metadata gate 报告 Rust package 中有 13 个 Core、6 个 public、2 个 separate-decision。未为 plugin exception 创建正式许可证文本。

## Extension 矩阵

| Extension | 进程模型 / transport | 编译依赖 | 能否使用专有许可证？* | 原因 |
|---|---|---|---|---|
| Worker | 独立进程；worker control IPC/UDS | cy-proto，可选 public SDK | 可以 | 使用文档化的进程协议 |
| Provider | 独立进程；有版本的 Provider UDS/gRPC | cy-kernel-contract + cy-proto | 可以 | Public Provider fact 与 wire contract |
| Service plugin | 独立 service；有版本的 service/control protocol | 仅 public protocol/SDK | 可以 | 无 Core link-time ABI |
| Hardware adapter | 独立 adapter process；Hardware Adapter v1 UDS | cy-kernel-contract + cy-proto | 可用于独立开发的 adapter | Public adapter SPI；参考实现另行决策 |
| Sandbox | 特权 Platform service；Core UDS/spawn | Core/internal API | 不适用任意 plugin 分类 | Enforcement 职责使其属于 Core |
| Node Agent | Platform 所有进程；认证 control UDS | Core implementation | 受 Platform Core policy 管理 | 拥有 Platform node lifecycle |
| Runtime Agent | Platform 所有进程；认证 control UDS | Core implementation | 受 Platform Core policy 管理 | 拥有 Platform runtime lifecycle |
| Future integration | 位于文档化 public seam 的独立进程 | 仅 public contract/SDK | 可以 | 必须独立开发，且不进入 Core link closure |

\* “能否使用专有许可证？”是架构设计判断，不是法律意见。该判断只适用于使用指定 public process interface 的独立开发 extension；它不会改变 Platform Core 修改版的许可证。

架构结论：PLUGIN_EXCEPTION_NOT_ARCHITECTURALLY_REQUIRED。当前受支持的 extension path 不需要链接 AGPL Core implementation crate。法律审查仍可能选择其他 distribution policy。

## 验证结果

### 架构与 package

| Gate | 结果 | 证据 |
|---|---|---|
| Public/Core dependency firewall | PASS | python3 tooling/ci/check-license-boundary.py |
| License metadata | PASS | 6 个 public、13 个 Core、2 个 separate-decision package |
| Public Proto mirror | PASS | bash tooling/ci/check-public-proto-sync.sh |
| 独立 Worker/Hardware consumer | PASS | bash tooling/acceptance/licensing-boundary/run.sh；dependency closure 不含 Core package |
| Public package 内容 | PASS | bash tooling/ci/check-public-packages.sh；6/6 package 与 verify 均通过 |
| Leaf cargo publish --dry-run | PASS | cy-kernel-contract、cy-proto、cy-manifest |
| 依赖型 cargo publish --dry-run | DEFERRED_REGISTRY | 前置 package 发布并完成 registry index 传播前，预期会遇到 crates.io 查找失败；没有上传 package |

Release DAG 与 registry propagation 流程见 docs/release/PUBLIC_PACKAGE_RELEASE_ORDER.md。Package verifier 只在本地 package 内容及编译验证中使用临时的 [patch.crates-io] mapping；这不构成 crate 已发布的证明。

### 本地 build 与 test

| 检查 | 结果 |
|---|---|
| cargo fmt --all -- --check | PASS |
| cargo check --locked --workspace --all-targets --all-features | PASS |
| cargo test --locked --workspace --all-features | PASS |
| cargo clippy --locked --workspace --all-targets --all-features -- -D warnings | PASS |
| uv run --locked python tooling/ci/verify.py --scope all-light | 本报告加入后为 PASS；此前仅因该报告链接尚未创建而受阻 |
| Architecture/governance/kernel-boundary check | PASS |
| Buf distributed workspace check | 本地因未安装 buf 而为 BLOCKED_BY_ENVIRONMENT；CI workflow action 会安装 Buf |

完整 Rust test run 包含预期会因需要 root 或第二个本地账号而被 ignore 的 test。不能仅因编译成功就声称 GPU 或特权 test 已通过。

### Security 就绪情况

SECURITY.md 和 docs/security/threat-model.md 记录了 trust、authentication、privilege 与 enforcement boundary。当前 sandbox 实现提供 cgroup v2 resource control、CPU/memory/PID/IO accounting 或 limit、BPF device policy、在可用时基于 pidfd 的跟踪，以及 cgroup-scoped cleanup。

它不提供 user/mount/network/PID namespace isolation、seccomp、capability dropping、完整的 host-filesystem isolation 或 syscall containment。不能将其宣传为恶意任意代码 containment，也不能称为完整的多租户 security sandbox。文档所述能力是有界的 cgroup-scoped lifecycle management 与 cleanup。

### 当前树与历史中的敏感数据审计

| 审计 | 结果 | 范围与限制 |
|---|---|---|
| 当前树高置信度扫描 | PASS | 跟踪文件与未被 ignore 的文件；扫描私钥、常见 cloud/token 格式、credential assignment 和敏感文件名模式；不输出 secret 值 |
| 可达历史高置信度扫描 | PASS | 对 --all refs 中 6,978 个可达 Git object 检查；未发现高置信度 credential pattern |
| 历史文件名审查 | PASS | 发现一个旧 infrastructure/cyrene-runtime-deployment/.env.example，归类为 OK_TO_PUBLISH_HISTORY：示例配置而非 credential material |
| Private endpoint 检查 | PASS | 未发现私有网络 literal；两个 localhost URL 是 test/acceptance endpoint |
| 成熟 secret scanner | NOT_INSTALLED | 本地没有 gitleaks 和 trufflehog；不声称检查覆盖率等价于这些 scanner |

当前预期 public ref 包含本地/远端 develop 和 main，以及 remote HEAD symbolic ref；本地 ref 集中没有 tag。未执行破坏性的 history rewrite。若未来更强的 scanner 发现真实历史 credential，必须先轮换 credential；history rewrite 还需单独授权并做好备份。

### 第三方许可证审计

cargo metadata --locked --all-features 报告 280 个外部 Cargo package，其中没有缺少 license/license_file metadata 的 package。锁定的 Python 集合有 19 个 package；17 个已安装 distribution 暴露 License、License-Expression 或 License-File metadata；另两个未安装项分别是本地 workspace package 和条件性 package。未发现被跟踪的 vendor 或第三方 source 目录。由于没有安装 cargo-deny，这属于 metadata/provenance 预检，不是法律上的 dependency 意见。

结果：PASS_WITH_TOOLING_LIMITATION；现有检查未发现仓库级冲突或复制 source。

## Clean-machine 与 hosted CI 就绪情况

| 区域 | 结果 | 解释 |
|---|---|---|
| README build/test 指引 | READY_WITH_DOCUMENTATION | 明确记录 CPU-only build/test、Linux/cgroup/NVIDIA 前置条件及发布前 runtime 状态 |
| 全新 Ubuntu clone/build/test/lifecycle | DEFERRED_UNTIL_PUBLIC | 在该私有仓库上下文中没有运行新的 GitHub-hosted Ubuntu runner |
| Hosted GitHub CI | DEFERRED_UNTIL_PUBLIC | 本地交付过程中未手动触发或监控；正常 push 后仓库 workflow 可能自动排队 |
| Azure pipeline | NOT_TRIGGERED | 仓库配置为 trigger: none 和 pr: none |

剩余 hosted gate 应使用未缓存的 ubuntu-24.04 environment，安装文档要求的前置条件（包括 Buf），build/test workspace，运行最小非 GPU worker path，并关闭所有 service。生产 sandbox mode 必须保留真实 privilege 和 cgroup 要求。

## 按优先级排列的剩余事项

### P0

未发现。

### P1

- 仓库公开后运行 hosted GitHub CI 和全新 Ubuntu lifecycle gate。这是有意要求公开之后取得的证据，不应伪装成本地 PASS。

### P2

- 在 hosted wire-check environment 安装并运行 Buf；本地缺少该工具只是环境限制。
- 若 release environment 提供 policy-grade 第三方许可证 scanner，则安装并运行；现有 Cargo/Python metadata check 已通过。
- 若未来要采用双重许可或广泛 relicensing，应取得法律意见并采用明确的 CLA/DCO policy。仓库当前没有捏造这类 policy。

### P3

- 在 release checklist 中保留 public package registry release 顺序与 propagation wait。
- 在 deployment environment 运行真实的特权/root/cgroup acceptance matrix，再声称 production sandbox 已就绪。

## 最终结论

READY_TO_MAKE_PUBLIC_WITH_POST_PUBLIC_CI

技术边界现已体现在 source layout、Cargo dependency metadata、CI gate、package verification、独立 consumer 和 runtime documentation 中。本任务可在不改变仓库可见性或发布 artifact 的情况下准备公开。公开后、声称 production release 之前，仍需 hosted CI、全新机器 lifecycle evidence 和 Buf environment check。
