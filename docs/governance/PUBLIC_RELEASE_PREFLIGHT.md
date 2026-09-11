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
