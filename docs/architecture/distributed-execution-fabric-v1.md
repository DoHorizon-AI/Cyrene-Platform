# Distributed Execution Fabric v1

Status: **V1 REFERENCE VERTICAL IMPLEMENTED**

Scope: Cyrene-Platform contracts, generic control-plane orchestration, execution
agents, and Artifact transfer.

Non-goal: Product semantics, global scheduling, a production SaaS identity
system, or a global data-delivery network.

状态：**v1 reference vertical 已实现**。本文只约束 Platform 的通用执行 Contract、
控制面编排、Execution Agent 与 Artifact 传输；不定义 Product 语义、全球调度、
生产级 SaaS 身份系统或全球数据分发网络。

## 1. Problem statement / 问题定义

Cyrene must execute the same Product-neutral `ExecutionPlan` on environments
with materially different privilege and persistence boundaries:

- a Windows or WSL workstation and an ordinary Linux host can run a durable
  host service;
- an owned GPU workstation can run the Kernel, sandbox adapters, and Node
  Agent;
- many GPU rental products expose only a Docker/Pod boundary and do not permit
  a host daemon, Docker socket, systemd, root, or an inbound port;
- provider-managed systems such as Kubernetes, Slurm, RunPod, Azure, and GCP
  may own workload creation and expose only provider observations;
- containers can stop normally, receive `docker kill`, be OOM-killed, be
  preempted, lose their host, lose their network, or be replaced by a new image;
- artifacts can be large, cross regions with unstable bandwidth, and traverse
  paths where GitHub, Hugging Face, or one object store is slow or unavailable;
- Navigator, browsers, CLIs, IDEs, and future mobile clients must address
  stable Product/Platform identities, never a Docker IP, container ID, GPU
  server IP, local path, or runtime generation.

Cyrene 必须在权限和持久性差异极大的环境中执行同一套 Product-neutral
`ExecutionPlan`。宿主机 daemon 只是一种部署模式；Docker-only GPU 实例、无宿主机
权限的 Pod 和 provider-managed workload 都是一等场景。容器、网络与数据源均可能
随时消失，因此连接、容器 ID、IP 和路径都不能成为长期身份或状态权威。

## 2. Current repository reality / 当前仓库现实

The v1 design is constrained by authorities already present on
`origin/develop`:

| Existing authority | Canonical location | Consequence for this design |
|---|---|---|
| Kernel semantic nouns, transitions, bounds, Lease/Fence and replay | `docs/contracts/kernel-semantic-contract-v1.md`, `contracts/proto/cyrene/semantic/v1/kernel_contract.proto`, `contracts/rust/cy-kernel-contract/` | No tenth Kernel noun and no second Lease, Fence, Worker, Operation, Capability, or Event authority. |
| Kernel RPC projection | `contracts/proto/cyrene/core/v1/kernel_authority.proto` | Host execution continues to use `KernelAuthorityService`. |
| Outbound agent control stream | `contracts/proto/cyrene/core/v1/node_control.proto`, `NodeControlService.Connect` | v1 adds an additive Execution Agent branch to this stream instead of creating another control protocol/service. |
| Host Agent | `agents/node/cy-node-agent/` | `HOST_AGENT` adapts this component; no second Node Agent is created. |
| Provider observations | `contracts/proto/cyrene/provider/v1/kernel_provider.proto`, semantic `ProviderSnapshot` | Provider-managed observation reuses Provider/Resource/Worker snapshots and reconciliation. |
| Product-neutral plan/run/attempt reconciliation | `sdk/python/cyrene_control_plane/`, `framework/jvm/domain/` and `framework/jvm/application/` | Product Run and Attempt remain above Kernel Operation and Runtime. |
| Generic process supervision | `cy-kernel-api::service`, `ServiceSupervisor`, `ProcessRuntime` | Runtime Agent reuses the lifecycle shape but remains an unprivileged outer agent, not a Kernel authority noun. |
| Package/binding runtime lifecycle | `framework/crates/cy-package-runtime/` | Package install/binding/runtime state is not copied into the fabric. |
| Artifact identity and local CAS | `contracts/rust/cy-manifest`, `contracts/schemas/manifests/artifact_*.schema.json`, `sdk/python/cyrene_artifacts/` | Transfer adds replicas/sessions/checkpoints around `ArtifactRef`; it does not redefine Artifact identity. |

当前仓库已经冻结 Kernel 的九个语义名词，并已有 Node Agent、ProviderSnapshot、
Product-neutral Attempt、package runtime 及 Artifact identity。Fabric 必须消费这些
authority；新增的 Attachment、Runtime session 和 Transfer session 都只是 Platform
编排对象或传输对象，不得进入 Kernel 语义账本。

Two current documents overstate implementation maturity: `docs/API.md` calls
the current Node Agent and control plane stable, while the normative Kernel
contract explicitly calls Node Agent and provider transport partial migration
projections. This design follows the normative contract and executable
evidence, not the optimistic status label.

现有 `docs/API.md` 对部分能力的成熟度描述高于规范 Contract。本文以 frozen Kernel
Semantic Contract 和可执行证据为准，并把未完成部分明确列为 extension seam。

## 3. Architecture model / 架构模型

`ExecutionAttachment` is a Platform orchestration contract describing how an
execution authority is reached. It is not a Kernel semantic noun.

```text
Execution Agent
├── Host Agent       -> existing cy-node-agent -> local KernelAuthority
└── Runtime Agent    -> cy-runtime-agent inside an unprivileged container

Provider Adapter     -> provider-owned create/stop/observe implementation
```

The frozen logical attachment types are:

| Attachment type | Meaning | Typical persistence |
|---|---|---|
| `HOST_AGENT` | Durable outbound Node Agent bridges the control plane to one node-local Kernel authority. | `PERSISTENT` |
| `CONTAINER_AGENT` | Runtime Agent runs inside the assigned container, dials out, stages artifacts, and supervises one child workload without host privileges. | `EPHEMERAL` |
| `PROVIDER_MANAGED` | A provider adapter creates/stops the workload and contributes provider observations; a Runtime Agent may still run inside the workload when available. | provider-defined, normally `EPHEMERAL` |

Every Node carries an explicit `persistent: bool`, orthogonal to `node_type`.
The older `PersistenceClass` wire field remains only as a compatibility
projection and admission rejects disagreement with the boolean. A persistent
Node transitions `ONLINE -> OFFLINE -> ONLINE` under the same `NodeId` and a
new `NodeSessionId`; an ephemeral Node may close as
`ONLINE -> LOST -> TERMINATED` and its replacement receives a new `NodeId`.
Examples:

- local WSL or Linux: `HOST_AGENT + PERSISTENT`;
- owned GPU workstation: `HOST_AGENT + PERSISTENT`;
- Vast/RunPod Docker-only rental: `CONTAINER_AGENT + EPHEMERAL`;
- Kubernetes: `PROVIDER_MANAGED + EPHEMERAL`, optionally with a Runtime Agent
  attachment inside the Pod.

`ExecutionAttachment` 描述控制面如何到达执行权威，不创造新的 Kernel 实体。
`HOST_AGENT` 复用现有 Node Agent；`CONTAINER_AGENT` 是容器内主动出站的无特权
Runtime Agent；`PROVIDER_MANAGED` 由 Provider Adapter 创建、停止并观察 workload。
`node_type` 与显式 `persistent` 正交，后者直接驱动断线、Lease 过期和重连行为。

Restart is also explicit: `NONE` for an unsupervised container,
`HOST_SUPERVISED` for a Host Agent, and `PROVIDER_SUPERVISED` for a Provider
Adapter. The contract never promises that a destroyed container can restart
itself.

## 4. Identity model / 身份模型

The following identities are distinct and never inferred from one another:

| Identity | Owner and lifetime |
|---|---|
| `UserId` | Identity provider; human lifetime. |
| `OrganizationId` | Account/authorization plane; organization lifetime. |
| `WorkspaceId` | Product/control-plane scope; workspace lifetime. |
| `NodeId` | Stable enrolled host identity. Existing `NodeRef.node_id`. |
| `NodeSessionId` | One authenticated Node Agent connection session. Existing `NodeWelcome.session_id`. |
| `RuntimeId` | Stable logical execution-runtime identity chosen by the control plane. |
| `RuntimeGeneration` | Non-zero incarnation of `RuntimeId`; represented by the existing semantic `Identity.generation` rules. |
| `OperationId` | Existing Kernel semantic `Operation.identity`; one bounded transition. |
| `AttemptId` | Existing Product-neutral control-plane Attempt; may span replacement Operations/Runtime generations according to Product retry policy. |
| `WorkloadIdentity` | Short-lived, scoped identity delegated to exactly one Runtime generation. |

`ProductRun != Attempt != Kernel Operation != Runtime != container`.
A container ID is provider-private observation only. Destroying a container
does not delete a Product Run, Attempt history, Artifact, or Runtime logical
identity. A replacement process/container uses a higher Runtime generation;
the old generation remains immutable history and cannot regain authority.

`RuntimeId` 与 `RuntimeGeneration` 在 wire 上复用 frozen semantic `Identity` 的
`id + generation` 规则，不新建另一套 identity engine。package runtime 中已有的
`RuntimeGeneration` 只管理 package binding 的本地 incarnation；若它运行于 Fabric
Runtime 中，映射关系由控制面记录，二者不得互相猜测。

## 5. Control model / 控制模型

### 5.1 One outbound control wire

The single v1 wire authority remains
`cyrene.core.v1.NodeControlService.Connect`, a Protobuf/gRPC bidirectional
stream. Protocol version 1 retains the existing `NodeHello` host projection.
Protocol version 2 adds an additive `ExecutionAgentHello` branch and generic
execution frames to the same envelopes. Existing field numbers are not
reused.

Every agent initiates an authenticated outbound connection. No agent requires
an inbound listener, VPN, Docker socket, or discoverable container IP.

唯一 Control Wire 仍是现有 `NodeControlService.Connect`。v2 只在同一 envelope
中增加 Execution Agent 分支；不会并行创建 `RuntimeControlService` 或第二套消息总线。

### 5.2 Session and frame rules

Every frame carries:

- a unique `frame_id` for idempotency;
- a strictly increasing sender `sequence_number`;
- `ack_sequence_number`, the highest contiguous peer sequence durably applied;
- the authenticated `session_id` after welcome;
- protocol and semantic compatibility selected during hello/welcome.

`Hello` includes attachment kind, persistence class, stable runtime identity,
agent version, offered protocol range, capabilities, optional complete
Provider inventory, resume token/cursor, and a bootstrap proof. `Welcome`
selects versions, establishes a new session, returns heartbeat/lease policy,
and acknowledges replay state.

The control vocabulary supports:

- enrollment and session establishment;
- capability advertisement and complete inventory;
- heartbeat and Lease renewal request/result;
- assignment and idempotent `AssignmentAck`;
- desired state, observed Runtime/Worker/Operation state, progress and
  structured semantic Events;
- provider observation and reconciliation hints;
- Artifact/log references, never bulk Artifact bytes;
- `StopCommand(command_id)` and idempotent `StopAck(command_id)`;
- reconnect and replay from the last contiguous acknowledged sequence.

A connection is transport state, not authority state. Reconnect creates a new
session. A resume cursor can recover unacknowledged observations only after the
control plane proves the same enrolled identity and active Runtime generation.
`GAP` or changed authority generation triggers snapshot reconciliation, using
the existing Kernel replay semantics rather than inventing event sourcing.
The Alpha reference control service keeps an initial enrollment pending until
the first authenticated post-Welcome frame, so a dropped Welcome can replay the
same grant and token without spending a second one-shot proof. The opaque token
is stable for that grant's lifetime. This pending/grant state remains in memory;
a control-service restart therefore requires production durable session state
or fresh enrollment rather than a fabricated fallback.

## 6. Lease, loss, and reconciliation / Lease、丢失与协调

The canonical semantic `Lease` and fence token are the only execution
authority. An Assignment carries an already-issued Lease; the Runtime Agent
can validate, renew, and present it but cannot mint, replace, or reinterpret
it. Host mode obtains it from the local `KernelAuthorityService`. Container
and provider-managed deployments must bind to exactly one conforming execution
authority implementation; the MVP control fixture uses the canonical
`cy-kernel-contract` projection and is explicitly not a second production
lease store.

Agent liveness is evaluated from four inputs:

1. desired Runtime state;
2. Agent observation;
3. Provider observation when available;
4. Lease observation, including expiry and fence.

A missed heartbeat alone never means “container crashed.” Reconciliation
classifies terminal observations as:

- `EXPECTED_TERMINATION`: desired state was `STOPPED`, or an accepted stop was
  in progress, and later Lease disappearance is consistent with that intent;
- `GRACEFUL_TERMINATION`: the child handled graceful termination and cleanup
  was observed;
- `UNEXPECTED_LOSS`: an active desired Runtime loses Lease authority without a
  provider explanation or final Agent observation;
- `EXTERNAL_TERMINATION`: the provider confirms stop/preemption/host action;
- `UNKNOWN_LOSS`: evidence is contradictory or insufficient.

If the Agent and Lease disappear while an independent Provider still reports
`RUNNING`, reconciliation yields a network-partition candidate rather than a
crash classification.

Runtime generation and Lease fence are both checked. A stale generation,
expired Lease, wrong holder, or wrong fence is rejected before any child
mutation. Newer observations are not overwritten by duplicate or out-of-order
frames. Events are evidence; reconciled authority state is truth.

Lease/fence 继续使用 Kernel Contract 的唯一规则。控制面必须结合 desired state、
Agent observation、Provider observation 与 Lease observation 做 reconciliation；
单次断线或 heartbeat 缺失不能直接等价为 crash。

## 7. Graceful stop / 优雅停止

Normal shutdown follows this frozen sequence:

1. control plane persists `DesiredRuntimeState = STOPPED`;
2. it sends `StopCommand(command_id, runtime, grace_period)`;
3. the Runtime Agent persists/deduplicates the command and returns
   `StopAck(command_id)`;
4. the Agent sends `SIGTERM` to the supervised child process group, allows a
   bounded drain/checkpoint window, flushes its observation/outbox, and uses
   forced termination only after the deadline;
5. the Agent reports the final observation if the channel is available.

If the final `RuntimeStopped` observation is lost, the combination of
`Desired=STOPPED + StopAck + subsequent Lease disappearance` is sufficient to
classify expected termination. A forced kill without those facts remains
unexpected/unknown until provider evidence resolves it.

正常停止以 persisted desired state 和幂等 command/ack 为依据；最终 stopped 消息
不是唯一事实来源。Agent 只监督 workload 子进程，workload 本身不承担平台连接。

## 8. Runtime update model / Runtime 更新模型

A `RuntimeProfile` is immutable and includes an image digest and environment
digest. Tags such as `latest`, an implicit `pip install --upgrade`, or a mutable
working directory are not Runtime identity.

Update is `new generation -> readiness -> switch -> drain old generation ->
cleanup`. Training and inference may use different profiles. A running
assignment is never silently rewritten by an image/package update.

Runtime 更新只能创建更高 generation，并在 readiness 后切换，再 drain 旧 generation。
运行中任务不得被隐式镜像或依赖升级改写。

## 9. Artifact model / Artifact 模型

The Artifact Plane remains separate from control. Existing `ArtifactRef` and
`ArtifactManifest` retain identity authority. `Node != ArtifactPeer`; a Node
may advertise a Peer capability, while an object-store gateway can be a Peer
without being a Node. v1 adds these transport records:

- `ArtifactPeer`: one policy/health-bearing transfer endpoint;
- `ArtifactReplica`: one provider/private acquisition location for an existing
  Artifact identity, with region, protocol, priority, expiry, and capabilities;
- `TransferPlan`: per-part source routing, permitting different Peers for
  different chunks from the first contract version;
- `TransferTicket`: short-lived Artifact/source/destination/chunk/expiry/byte
  scoped authorization issued after policy selection;
- `TransferSession`: one resumable transfer of one Artifact to one target;
- `TransferPart`: bounded byte range and expected per-part SHA-256;
- `TransferCheckpoint`: durably completed parts and manifest/session identity.

Product resources contain only `artifact://sha256/...`, digest, manifest and
logical metadata. They never contain `C:\...`, `/home/...`, `/mnt/...`, a
presigned URL, or provider credentials. Replica URLs and local paths live only
inside Artifact provider/transfer state.

Control stream 只传 Artifact identity、manifest/digest 与短期 scoped ticket；大型
数据走独立 Artifact Plane。一个 Artifact 可以有多个 Replica，地址不是身份。

Core v1 `RuntimeAssignment` projects remote inputs through
`ArtifactTransferSpec` and already-local inputs through the additive
`ArtifactLocalInput`. A `VerifiedLocal` quote must map to an exact immutable
identity projection; the wire never carries a local path, locator, transfer
ticket, or credential. Before workload startup, Runtime Agent derives the CAS
entry only from the digest and re-reads the complete file to verify SHA-256 and
size. Missing or mismatched content rejects the Assignment without a running
child. This projection does not move Artifact locality, replica, or ticket
authority into Framework.

## 10. Artifact transfer MVP / Artifact 传输 MVP

The reference transfer provider implements HTTPS byte ranges with bounded
concurrency, per-part SHA-256 verification, a durable checkpoint, resumable
retry, whole-Artifact digest verification, and atomic publish by rename/link
only after complete verification. A partial destination is never published as
the Artifact path.

`ArtifactSourceResolver -> TransferPlanner -> TransferPlan` is owned by one
Artifact-plane coordinator. It filters authorization, residency, trust domain,
classification, and policy before considering health, latency, bandwidth, or
cost. The MVP planner routes all chunks to one selected seed, while the wire
and Rust plan accept per-chunk multi-Peer routes. The contract permits later
regional mirrors, LAN peers, P2P, Dragonfly, or cloud-native providers. v1 does
not implement a global CDN, P2P discovery, BitTorrent-like exchange, or a
Dragonfly scheduler.

第一版只实现 HTTPS Range、多 part 有界并发、part/full digest、checkpoint resume 和
atomic publish。区域副本与 P2P 是 provider extension，不进入 Artifact identity。

## 11. External source acquisition / 外部来源导入

Runtime startup must not depend directly on `git clone github.com` or a live
Hugging Face download. The long-term flow is:

```text
ExternalSource -> AcquisitionProvider -> SourceImportJob
               -> SourceSnapshot -> internal Artifact -> Replica/cache
```

`ExternalSource` is a provider-specific requested source; `SourceSnapshot` is
an immutable, digest-bound result; `SourceImportJob` is a generic operation
reference; `AcquisitionProvider` is a replaceable port. The reference HTTP
provider streams an HTTP/HF-like fixture into a temporary file, verifies its
digest, and atomically publishes a canonical internal Artifact. It does not
claim China/US regional mirrors.

Runtime 启动只消费内部 Artifact。GitHub/Hugging Face 等外部来源先由可替换
AcquisitionProvider 导入并冻结为 SourceSnapshot，再进入 Artifact/Replica 流程。

## 12. Account discovery, enrollment, and security / 账号发现、注册与安全

The architecture allows `same account -> discover Organization -> discover
Workspace -> enroll device/runtime -> establish connection`, but v1 does not
implement the complete account system.

The MVP accepts a single-use development enrollment token over authenticated
TLS and exchanges it for a short-lived Workload identity scoped to one
Workspace, Runtime identity/generation, permitted capabilities, Artifact
actions, and expiry. The raw token is not journaled or returned in events.

The contract leaves a production `EnrollmentProvider` seam for OIDC, OAuth
Device Authorization, SSO, and enterprise approval. User identity, Node
identity, and Workload identity remain separate. A container never receives a
long-lived Organization credential, user password, cloud-admin credential, or
database-admin secret. Artifact access uses separate short-lived scoped
credentials.

开发 token 只是明确标注的 reference implementation，不伪装为生产身份方案。容器只
持有短期 Workload identity 和 scoped Artifact credential。

## 13. Connectivity provider seam / 连接 Provider seam

`ConnectivityProvider` supplies an authenticated outbound control channel and,
optionally, direct Artifact/Endpoint connectivity. Identity never depends on a
Tailscale IP, container IP, host IP, relay address, or cloud-private address.

The contract modes are `LOCAL`, `LAN_DIRECT`, `DIRECT`, `OVERLAY`, and `RELAY`.
The MVP implements outbound-only `LOCAL` and relay-first `RELAY` providers over
TLS/gRPC plus ordinary HTTPS. Future providers may be Tailscale-integrated,
native direct, or cloud-private-network implementations. v1 does not implement
WireGuard, STUN, ICE, NAT traversal, or a global relay network.

## 14. Reliability invariants / 可靠性不变量

1. Commands and observations are idempotent by stable ID.
2. Duplicate delivery is accepted only as an identical replay.
3. Sender sequence is monotonic; receiver ack is the highest contiguous
   durably applied sequence.
4. Out-of-order observations never advance authority.
5. Runtime generation and Lease fence are checked independently.
6. Reconnect changes session identity, not Runtime identity.
7. Process/container loss is reconciled from desired, Agent, Provider, and
   Lease facts.
8. Events/logs are observations, never the sole state store.
9. One component owns retry/restart policy for a logical Runtime generation.
10. Product-neutral wire fields contain no TrainingRun, Dataset, ModelVersion,
    Deployment, Catalyst, Echo, Reactor, Exchange, Navigator, or Yield types.

所有消息处理必须容忍 duplicate、reconnect、out-of-order、stale generation、
process crash 与 network partition。最终状态来自 authority + reconciliation。

## 15. Ownership matrix / Ownership 矩阵

| Concern | One authority | Consumers / projections | Must not own |
|---|---|---|---|
| Lease/Fence | Kernel semantic authority (`cy-kernel-contract` / conforming `KernelAuthority`) | Host Agent, Runtime Agent validation, provider reconciler | Product, Runtime Agent, event store, Artifact service |
| Runtime identity/generation | Fabric control-plane Runtime record using semantic `Identity` rules | Runtime Agent session, Provider Adapter | container ID, Node session, package catalog |
| Worker/Operation lifecycle | Kernel semantic authority | Fabric observations and Product Attempt mapping | package install state, connection state |
| Capability | semantic Capability + existing resolver/catalog authorities | advertisement snapshots | agent-private ad-hoc schema |
| Artifact identity | `cy-manifest` / Artifact schemas | Python/Rust SDK projections and transfer providers | URL, path, TransferSession |
| Execution target placement | `cy-execution-fabric` deterministic planner over caller-supplied observations | control-plane assignment workflow | Resource reservation, Lease issuance, Artifact replica/ticket selection |
| Wire | `NodeControlService.Connect` + `node_control.proto` | Host and Runtime agents | a second runtime-specific service |
| Package/runtime lifecycle | `cy-package-runtime` | Fabric Runtime may host a binding | Fabric control session, Product binding state |
| Product Run/Attempt | Product domain/state authority | control-plane execution projections | Kernel, Runtime Agent, container provider |

`DUPLICATE_AUTHORITIES=0` is a release gate, not a documentation aspiration.

## 16. Implementation map / 实现映射

### ExecutionAttachment

- **Concept:** How the control plane reaches execution without changing Kernel nouns.
- **Canonical authority:** Platform orchestration record; wire projection uses semantic `Identity`.
- **Current existing implementation:** protocol-v1 Host plus protocol-v2 Runtime
  attachment, identity, persistence, restart, observation, and assignment
  projections.
- **New code needed:** NO for the Alpha reference seam; production identity and
  replicated recovery composition remain required.
- **Proposed location:** `contracts/proto/cyrene/core/v1/node_control.proto`; pure admission/reconciliation in `framework/crates/cy-execution-fabric/`.
- **Why this location:** Reuses the only existing agent stream and keeps policy outside Kernel.
- **Dependencies:** semantic contract, existing Node control envelopes.
- **Must NOT own:** Lease, Worker, Operation, Product Run, Provider registry.
- **MVP implementation:** enum, persistence class, runtime/account scope refs, validation.
- **Future extension:** attachment selection/scheduling policy.

### Runtime Agent

- **Concept:** Unprivileged in-container outbound agent supervising one child workload.
- **Canonical authority:** Runtime assignment in Fabric control state; child authority remains Lease/Fence-bound.
- **Current existing implementation:** `agents/runtime/cy-runtime-agent` provides
  an unprivileged outbound mTLS agent, canonical assignment/Lease admission,
  resumable Artifact staging, child-process supervision, and observation
  delivery. Its grant-lifetime resume token is atomically persisted under the
  exact Runtime generation and NodeRef namespace in a locked, symlink-resistant
  owner-only state directory.
- **New code needed:** accepted-assignment/workload recovery and a supervised
  restart mode; the current `CONTAINER_AGENT` declares `RestartCapability::None`.
- **Proposed location:** `agents/runtime/cy-runtime-agent/`.
- **Why this location:** Sibling of, not replacement for, the existing Host/Node Agent.
- **Dependencies:** `cy-proto`, `cy-kernel-contract`, `cy-execution-fabric`, Artifact transfer SDK.
- **Must NOT own:** host resources, Docker daemon, Product semantics, Lease issuance, package installation.
- **MVP implementation:** `cy-runtime-agent run -- <command>`, outbound TLS, child process group, observations, reconnect, stop.
- **Future extension:** richer checkpoints, Windows container process boundary, multiple isolated child slots.

### Execution target placement

- **Concept:** Select one execution Node from current, Product-neutral resource,
  lifecycle, policy, cost, and Artifact locality evidence.
- **Canonical authority:** `cy-execution-fabric` owns only the deterministic
  placement decision and explanation. Kernel owns resources and Lease issuance;
  the Artifact Plane owns replica, Peer, ticket, and transfer-plan selection.
- **Current implementation:** `cy-execution-fabric` 0.2 exposes
  `plan_execution_placement` and `place_execution_target`. The planner consumes
  canonical `ProviderSnapshot`, `ResourceQuery`, capability contracts,
  `ArtifactRef`, and policy-scoped, time-bounded Artifact Plane quotes.
- **Why this location:** Placement composes existing authorities without moving
  cloud-vendor behavior into Framework and without making observations binding.
- **Must NOT own:** Provider adapters, Resource allocation, Lease/Fence,
  Artifact replicas/tickets/transfer plans, Product retry policy, or a global
  scheduler.
- **MVP evidence:** deterministic ordering, rejection reason codes, real
  provider resource classes, stale Provider generations, Artifact locality,
  HTTPS/capability enforcement, transfer cost/deadline, quote scope/expiry, and
  duplicate Node rejection are covered by the placement TCK.
- **Current limit:** `ResourceMatchEvidence` is read-only snapshot evidence, not
  a reservation. One placement request carries exactly one `ResourceQuery`
  because the canonical Kernel API currently acquires one Lease from one query.
  Atomic CPU + RAM + accelerator bundles require a Kernel contract extension;
  Framework must not emulate them with independently acquired Leases.
- **Integration seam:** `cy-execution-control` routes the selected `NodeRef`
  through its authenticated Host Agent session, sends the existing typed
  semantic `AcquireLease` command with the exact placement query, binds only
  the Kernel-returned Lease into `RuntimeAssignment`, and waits for the exact
  Runtime generation's `AssignmentAck`. The controller never mints or owns a
  Lease. A caller-injected durable intent ledger stores only the immutable
  payload digest, transition state, and read-only Node/Lease identity/fence
  evidence; Resource ownership remains exclusively in Kernel authority. A
  timeout after command delivery is classified
  `UNKNOWN_REQUIRES_RECONCILIATION`; it never changes the idempotency key and
  blindly acquires again.
- **Integration evidence:** `execution_assignment_tck` uses the production
  NodeControl service state machine, real `cy-node-agent` session/bridge, a
  real Kernel authority service over peer-credential-authenticated UDS, and
  the real Runtime assignment admission validator. It proves one selected
  Node, one canonical Lease/fence, one accepted assignment, duplicate dispatch
  suppression, canonical release/resource reuse, and exact idempotent Lease
  rollback through a replacement Host session on the same Node epoch. Its
  Runtime peer is a protocol harness. Separately, `mtls_runtime_agent_tck` uses
  locally issued CA/server/Host/Runtime certificates, the production
  fingerprint authenticator, real `cy-node-agent`, a peer-credential-authenticated
  Kernel UDS, `LinuxSystemProvider`, the real `cy-runtime-agent` and child
  process, a reopened file intent ledger, and a fresh Runtime Agent task/state
  invocation using its persisted grant-lifetime resume token. The Agent and
  control service are real production implementations composed inside one test
  OS process; this is not proof of an Agent OS-process restart, Docker, or a
  multi-container deployment. It does not restart `ExecutionControlService`;
  resume grants/tokens and accepted assignment bindings there remain in memory.
- **0.2 migration:** callers must construct canonical Provider/resource
  snapshots and Artifact placement quotes instead of passing ad-hoc target
  scores. The public request/candidate/result structs are intentionally a
  breaking 0.1-to-0.2 change.

执行目标 placement 只对调用方提供的当前观测做确定性决策与解释。ProviderSnapshot
匹配不是资源预留；只有 KernelAuthority 能签发 Lease。Artifact quote 必须由 Artifact
Plane 基于同一 policy scope 生成，Framework 不接触 replica、ticket 或 TransferPlan。
当前单个 `ResourceQuery` 与单 Lease API 对齐；CPU、内存、加速器的原子组合资源需要
先扩展 Kernel Contract，不能在 Framework 内用多个独立 Lease 假装原子性。

### Host Agent seam

- **Concept:** Persistent host bridge to the local Kernel.
- **Canonical authority:** Existing `cy-node-agent` plus local `KernelAuthorityService`.
- **Current existing implementation:** `agents/node/cy-node-agent/`.
- **New code needed:** NO for MVP; additive protocol-v2 adapter later.
- **Proposed location:** existing `agents/node/cy-node-agent/`.
- **Why this location:** It already owns outbound mTLS, Node session fencing, and typed Kernel forwarding.
- **Dependencies:** local Kernel UDS, Node control wire.
- **Must NOT own:** Kernel Lease state, hardware adapter internals, Runtime child process in container-only mode.
- **MVP implementation:** contract mapping and conformance fixture.
- **Future extension:** emit generic Execution Agent frames on protocol v2.

### Provider-managed seam

- **Concept:** Provider creates/stops workload and contributes independent observations.
- **Canonical authority:** semantic Provider/ProviderSnapshot plus Provider Adapter private handle.
- **Current existing implementation:** local `KernelProviderService` projection.
- **New code needed:** YES, reference seam only.
- **Proposed location:** reuse `contracts/proto/cyrene/provider/v1/kernel_provider.proto`; Fabric reconciliation in `framework/crates/cy-execution-fabric/`.
- **Why this location:** Provider observation semantics already exist and are fenced by provider generation.
- **Dependencies:** ProviderSnapshot, Runtime assignment, Lease observation.
- **Must NOT own:** Product state or Kernel Lease mutation.
- **MVP implementation:** `FakeProvider` observation in acceptance tests.
- **Future extension:** RunPod, Vast, Kubernetes, Slurm, Azure, GCP adapters.

### Control Wire

- **Concept:** Authenticated session, commands, observations, ack/replay.
- **Canonical authority:** `NodeControlService.Connect` and `node_control.proto`.
- **Current existing implementation:** Rust `cy-execution-control` owns the
  protocol-v1 Host and protocol-v2 Runtime session state machine. The former
  registration-only JVM inbound service and its `activeNodes` registry are
  retired, so there is one source implementation owner.
- **New code needed:** production composition for external identity/enrollment
  and durable multi-replica session state; no second NodeControl implementation.
- **Proposed location:** `contracts/proto/cyrene/core/v1/node_control.proto`; generated by `contracts/rust/cy-proto`.
- **Why this location:** Prevents a second wire authority.
- **Dependencies:** semantic contract and Artifact identity projection.
- **Must NOT own:** bulk data, credentials in events, authority snapshots as event-only state.
- **MVP implementation:** Execution Agent hello/welcome, assignment, ack, observation, renew, stop, sequence/ack.
- **Future extension:** compression and additional optional observation payloads.

### Enrollment

- **Concept:** Bootstrap to short-lived workload identity.
- **Canonical authority:** Future identity provider; MVP development verifier.
- **Current existing implementation:** Node mTLS file identity only.
- **New code needed:** YES.
- **Proposed location:** wire proof in `node_control.proto`; verifier port in `cy-execution-fabric`; fixture verifier in acceptance support.
- **Why this location:** Authentication stays at the transport/control boundary.
- **Dependencies:** TLS peer identity, account scope, Runtime generation.
- **Must NOT own:** user password or long-lived Organization OAuth credentials.
- **MVP implementation:** single-use development token, redacted persistence, expiring Workload identity.
- **Future extension:** OIDC, OAuth Device Authorization, SSO, enterprise approval.

### Lease/fence integration

- **Concept:** Time-bounded execution authority and stale mutation rejection.
- **Canonical authority:** frozen Kernel Semantic Contract v1.
- **Current existing implementation:** Kernel authority, resource manager, Node bridge.
- **New code needed:** YES, consumption/validation only.
- **Proposed location:** `cy-execution-fabric` validation and Runtime Agent assignment admission.
- **Why this location:** No new Lease type/store is required.
- **Dependencies:** semantic Lease, Worker holder, expiry clock, fence.
- **Must NOT own:** Lease issuance or replacement in Runtime Agent.
- **MVP implementation:** renew request/result, expiry transition, stale generation/fence rejection.
- **Future extension:** production remote authority routing and HA storage.

### Runtime state observation

- **Concept:** Desired/observed runtime and termination evidence.
- **Canonical authority:** Fabric Runtime record reconciled with semantic Worker/Operation/Lease.
- **Current existing implementation:** Product Attempt observation and node-local Worker state separately.
- **New code needed:** YES.
- **Proposed location:** protocol-v2 observation frames and `cy-execution-fabric` reconciler.
- **Why this location:** State composition belongs above Kernel and below Product policy.
- **Dependencies:** Agent, Provider, Lease, desired state.
- **Must NOT own:** Product terminal semantics or a second Event log.
- **MVP implementation:** state/progress/reason, termination classification, FakeProvider reconciliation.
- **Future extension:** policy-specific remediation and scheduler feedback.

### Runtime generation

- **Concept:** Immutable Runtime incarnation and update fencing.
- **Canonical authority:** Fabric Runtime record using semantic `Identity` generation.
- **Current existing implementation:** semantic generation rules and package-binding `RuntimeGeneration`.
- **New code needed:** YES, mapping and validation; no new identity engine.
- **Proposed location:** wire `RuntimeRef`/`RuntimeProfile`, `cy-execution-fabric`.
- **Why this location:** Keeps Runtime above Kernel without overloading container identity.
- **Dependencies:** image/environment digest, assignment, Lease.
- **Must NOT own:** package install identity or container ID.
- **MVP implementation:** reject zero/stale generation; prove replacement generation fences old agent.
- **Future extension:** readiness switch, drain, rollback coordination.

### Graceful stop

- **Concept:** Desired stop, idempotent command/ack, bounded child termination.
- **Canonical authority:** persisted desired Runtime state plus command state; Worker/Lease final authority remains Kernel semantic.
- **Current existing implementation:** node-local ServiceSupervisor and Worker stop paths.
- **New code needed:** YES.
- **Proposed location:** wire messages, `cy-execution-fabric` command state, `cy-runtime-agent` process supervisor.
- **Why this location:** Separates remote intent from local signal/cleanup mechanics.
- **Dependencies:** process groups, grace deadline, outbox flush.
- **Must NOT own:** Product cancellation policy.
- **MVP implementation:** SIGTERM, deadline escalation, StopAck/final observation.
- **Future extension:** workload checkpoint hooks.

### Reconnect/replay

- **Concept:** Recover a control channel without reviving stale authority.
- **Canonical authority:** session sequence/ack plus existing Kernel cursor semantics.
- **Current existing implementation:** Node session sequence and opaque resume token; no durable Agent outbox.
- **New code needed:** YES.
- **Proposed location:** protocol-v2 envelope fields, `cy-runtime-agent` bounded outbox/checkpoint, Fabric session state.
- **Why this location:** Extends the current channel rather than adding event sourcing.
- **Dependencies:** enrolled identity, Runtime generation, ack cursor.
- **Must NOT own:** Runtime or Lease identity.
- **MVP implementation:** forced stream disconnect, reconnect, replay/dedupe proof.
- **Future extension:** replicated session store and bounded history compaction.

### Artifact transfer

- **Concept:** Provider-neutral resumable movement of existing Artifact identity.
- **Canonical authority:** existing `ArtifactRef`/`ArtifactManifest`.
- **Current existing implementation:** `cy-artifact-transfer` provides the
  canonical Rust projection, policy-scoped source/Peer planner, scoped ticket
  boundary, HTTPS Range transfer, bounded concurrency, part/full digest
  verification, durable checkpoints, resume, and atomic publication. The
  Runtime Agent consumes that implementation, requires an Artifact Plane source
  projection, and invokes a ticket verifier before any network transfer. The
  bundled symmetric verifier is explicitly limited to development and
  conformance; production must inject managed verification authority without
  giving the Agent ticket-issuance power.
- **New code needed:** production Artifact directory/ticket issuers. The
  canonical local-input projection is implemented and remains identity-only;
  `VerifiedLocal` content is reverified from the Runtime Agent CAS before
  workload startup.
- **Proposed location:** `sdk/rust/cy-artifact-transfer/`; Runtime Agent consumes it; schemas remain under `contracts/schemas/`.
- **Why this location:** Transfer is a reusable SDK/data-plane concern, not Kernel or Product logic.
- **Dependencies:** HTTPS client, SHA-256, ArtifactRef, atomic filesystem operations.
- **Must NOT own:** Artifact identity, Product paths, global replica scheduling.
- **MVP implementation:** Range, bounded concurrency, part/full verification, checkpoint, resume, atomic publish.
- **Placement projection:** the Artifact Plane may derive a conservative,
  authorization-bounded `TransferEstimate` and project only neutral scalar
  metrics into an `ArtifactPlacementQuote`; `cy-execution-fabric` has no normal
  dependency on the transfer SDK and cannot select replicas or mint tickets.
- **Future extension:** regional, LAN, P2P, Dragonfly, cloud-native providers.

### AcquisitionProvider

- **Concept:** Convert an external mutable source into an internal immutable Artifact.
- **Canonical authority:** acquisition port plus Artifact identity after import.
- **Current existing implementation:** `cy-artifact-transfer::AcquisitionProvider`
  plus a reference HTTP importer that publishes a digest-verified immutable
  `SourceSnapshot`.
- **New code needed:** production Git/Hugging Face and regional acquisition
  adapters only; they must remain outside Runtime startup.
- **Proposed location:** `sdk/rust/cy-artifact-transfer/src/acquisition.rs`.
- **Why this location:** Acquisition feeds the Artifact Plane and stays out of Runtime startup.
- **Dependencies:** Artifact provider and generic Operation reference.
- **Must NOT own:** Runtime launch, Product dataset/model semantics, external credential persistence.
- **MVP implementation:** typed seam and fake snapshot proof.
- **Future extension:** GitHub/Hugging Face/mirror importers.

### ConnectivityProvider

- **Concept:** Replaceable outbound connectivity establishment.
- **Canonical authority:** Fabric connectivity port; identity remains independent.
- **Current existing implementation:** direct tonic TLS in Node Agent.
- **New code needed:** YES, seam plus direct provider.
- **Proposed location:** `framework/crates/cy-execution-fabric/src/connectivity.rs`; client use in Runtime Agent.
- **Why this location:** Connectivity policy is Platform orchestration, not Kernel or Product state.
- **Dependencies:** authenticated transport configuration.
- **Must NOT own:** Node/Runtime identity or lease state.
- **MVP implementation:** direct outbound TLS/gRPC control and HTTPS Artifact paths.
- **Future extension:** relay-only, Tailscale, native direct, cloud private network.

## 17. MVP reference vertical and acceptance / MVP 纵向切片与验收

The implementation sequence fixed by this document is:

1. additive wire projection and contract tests;
2. pure Fabric admission/reconciliation library;
3. unprivileged Runtime Agent supervising one child;
4. resumable HTTPS Artifact transfer SDK;
5. reference control/provider/artifact fixtures;
6. real Docker acceptance for enrollment, capability advertisement, running
   observation, heartbeat/renewal, reconnect, graceful stop, `docker stop`,
   `docker kill`, Lease expiry, generation fencing, stale rejection, and
   interrupted transfer resume with atomic publication after SHA-256 checks of
   every part and the complete Artifact.

The Docker fixture uses development enrollment tokens, locally issued test
certificates, and the development Artifact ticket authority. Its HTTPS source
accepts only the signed ticket carried by the Runtime Agent. These prove the
fixture's transport and authorization flow, not production certificate-to-Agent
identity binding or a production ticket issuer. It must not skip Docker fault
injection when Docker is available and must report exact pass/fail/skip counts.
Unit/fake tests do not substitute for the real Docker acceptance.

The in-process `mtls_runtime_agent_tck` is an additional executable boundary
proof for fingerprint-bound mTLS identity, Host-to-Kernel UDS, real Runtime
Agent workload launch, a fresh Agent task/state invocation, durable intent
reopen, and canonical release. It complements but does not replace the Docker
fault matrix or claim an OS-process restart.

本文冻结后的实现顺序不会在编码过程中重新分配 ownership。FakeProvider 只证明
Provider/Agent/Lease observation 可组合；真实 Docker kill、reconnect、Lease expiry 和
Artifact resume 才构成最终容器模式证据。

## 18. Deferred extensions / 延后能力

### Required next

- production OIDC/device enrollment and Workload identity issuer;
- production durable Fabric session/runtime store, resume grant/token state,
  accepted assignment binding, and HA reconciliation; the current Agent token
  survives a fresh Agent invocation, but neither an OS-process restart nor a
  control-service restart is established by the in-process TCK;
- Runtime accepted-assignment/workload persistence and a non-`None` restart
  contract before claiming workload recovery across Agent process loss;
- canonical reconciliation readers for unknown Acquire/Assignment/Release
  outcomes; the durable intent ledger already prevents blind redispatch, but
  only Kernel events and Runtime observations can resolve an unknown outcome;
- a canonical atomic resource-bundle contract before jointly reserving CPU,
  RAM, and accelerators;

### Optional scale-up

- regional Artifact replica selection and mirrors;
- LAN peer, P2P, and Dragonfly-style transfer providers;
- multi-runtime placement and global scheduler policy.

### Provider-specific

- RunPod, Vast, Kubernetes, Slurm, Azure, and GCP adapters;
- durable host Node Agent packaging for additional operating systems.

### Network infrastructure

- relay fleet, Tailscale provider, native direct connectivity;
- STUN/ICE/NAT traversal only if later evidence justifies it.

These items do not fail v1 when the frozen seams, reference vertical, and
fault-injection proofs pass.
