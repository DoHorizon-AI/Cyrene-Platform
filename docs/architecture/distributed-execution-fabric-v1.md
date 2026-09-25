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
| Product-owned run/attempt reconciliation | Product repositories; Yield owns its training implementation | Product Run and Attempt remain above Kernel Operation and Runtime and are not Platform state. |
| Generic process supervision | `cy-kernel-api::service`, `ServiceSupervisor`, `ProcessRuntime` | Runtime Agent reuses the lifecycle shape but remains an unprivileged outer agent, not a Kernel authority noun. |
| Package/binding runtime lifecycle | `framework/crates/cy-package-runtime/` | Package install/binding/runtime state is not copied into the fabric. |
| Artifact identity and local CAS | `contracts/rust/cy-manifest`, `contracts/schemas/manifests/artifact_*.schema.json`, `sdk/python/cyrene_artifacts/` | Transfer adds replicas/sessions/checkpoints around `ArtifactRef`; it does not redefine Artifact identity. |

当前仓库已经冻结 Kernel 的九个语义名词，并已有 Node Agent、ProviderSnapshot、
Product-neutral Attempt、package runtime 及 Artifact identity。Fabric 必须消费这些
authority；新增的 Attachment、Runtime session 和 Transfer session 都只是 Platform
编排对象或传输对象，不得进入 Kernel 语义账本。

The normative Kernel contract and executable evidence now record the current
projection status explicitly. Node Agent finite history forwarding is complete;
continuous observation is intentionally owned by the Core gRPC stream because
the Node control envelope is one-command/one-result. Hardware and Sandbox
Adapters are complete for their local fact/process boundaries and do not own
Event history. Cross-host identity, systemd recovery and privileged deployment
acceptance remain deployment-validation work, not API naming gaps.

现行规范 Contract 与可执行证据已经明确记录各投影状态：Node Agent 的有限历史转发已
完成，连续观察由 Core gRPC stream 负责；Hardware/Sandbox Adapter 只负责各自事实与进程
边界，不拥有 Event history。跨主机身份、systemd 恢复和特权部署验收仍属于部署验证，
不是 API 命名缺口。

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
`ArtifactRef` retains identity authority. `Node != ArtifactPeer`; a Node
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
| Execution target placement | `cy-execution-fabric` deterministic planner over caller-supplied observations | control-plane assignment workflow | Resource admission, Lease issuance, Artifact replica/ticket selection |
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
  an admission. One placement request carries exactly one `ResourceQuery`
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
- **Canonical authority:** the existing `ArtifactRef` contract.
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
---

<!-- Chinese Translation / 中文翻译 -->

# Distributed Execution Fabric v1

状态：**V1 参考纵向已实现**

范围：Cyrene-Platform 契约、通用控制面编排、执行 Agent 和 Artifact 传输。非目标：Product 语义、全局调度、生产级 SaaS 身份系统或全球数据分发网络。

## 1. 问题定义

Cyrene 必须在权限和持久化边界差异很大的环境中执行同一份与 Product 无关的 `ExecutionPlan`：

- Windows 或 WSL 工作站以及普通 Linux host 可以运行持久 host service；
- 自有 GPU 工作站可以运行 Kernel、sandbox adapter 和 Node Agent；
- 许多 GPU 租用服务只开放 Docker/Pod 边界，不允许 host daemon、Docker socket、systemd、root 或入站端口；
- Kubernetes、Slurm、RunPod、Azure 和 GCP 等 Provider-managed 系统可能拥有 workload 创建权限，并且只提供 Provider observation；
- container 可能正常停止、被 `docker kill` 或 OOM kill、被抢占、丢失 host/网络，或被新 image 替换；
- Artifact 可能很大，跨不稳定带宽区域传输，也可能经过 GitHub、Hugging Face 或 object store 缓慢/不可用的路径；
- Navigator、browser、CLI、IDE 和未来 mobile client 必须使用稳定 Product/Platform identity 寻址，不能依赖 Docker IP、container ID、GPU server IP、本地路径或 Runtime generation。

宿主机 daemon 只是部署模式之一。Docker-only GPU instance、无宿主机权限的 Pod 和 Provider-managed workload 都是一等场景。container、network 和 data source 随时可能消失，因此 connection、container ID、IP 和 path 不能成为长期身份或状态 authority。

## 2. 当前仓库现实

v1 设计受限于 `origin/develop` 上已经存在的 authority：

| 已有 authority | 规范位置 | 对本设计的影响 |
|---|---|---|
| Kernel semantic 名词、状态转换、边界、Lease/Fence 与 replay | `docs/contracts/kernel-semantic-contract-v1.md`、`contracts/proto/cyrene/semantic/v1/kernel_contract.proto`、`contracts/rust/cy-kernel-contract/` | 不增加第十个 Kernel 名词，也不新增 Lease、Fence、Worker、Operation、Capability 或 Event authority。 |
| Kernel RPC projection | `contracts/proto/cyrene/core/v1/kernel_authority.proto` | Host execution 继续使用 `KernelAuthorityService`。 |
| Agent 出站 control stream | `contracts/proto/cyrene/core/v1/node_control.proto`、`NodeControlService.Connect` | v1 在现有 stream 中增加兼容的 Execution Agent 分支，不另建 control protocol/service。 |
| Host Agent | `agents/node/cy-node-agent/` | `HOST_AGENT` 适配现有组件，不创建第二个 Node Agent。 |
| Provider observation | `contracts/proto/cyrene/provider/v1/kernel_provider.proto`、semantic `ProviderSnapshot` | Provider-managed observation 复用 Provider/Resource/Worker snapshot 和 reconciliation。 |
| Product-owned run/attempt reconciliation | Product 仓库；Yield 拥有训练实现 | Product Run 和 Attempt 留在 Kernel Operation 与 Runtime 之上，不是 Platform 状态。 |
| 通用进程监管 | `cy-kernel-api::service`、`ServiceSupervisor`、`ProcessRuntime` | Runtime Agent 复用生命周期形态，但仍是无特权外层 Agent，不是 Kernel authority 名词。 |
| Package/binding runtime lifecycle | `framework/crates/cy-package-runtime/` | Fabric 不复制 package install/binding/runtime state。 |
| Artifact identity 和本地 CAS | `contracts/rust/cy-manifest`、`contracts/schemas/manifests/artifact_*.schema.json`、`sdk/python/cyrene_artifacts/` | Transfer 围绕 `ArtifactRef` 增加 replica/session/checkpoint，不重定义 Artifact identity。 |

当前仓库已冻结 Kernel 的九个 semantic 名词，并已有 Node Agent、ProviderSnapshot、Product-neutral Attempt、package runtime 和 Artifact identity。Fabric 必须消费这些 authority；新增 Attachment、Runtime session 和 Transfer session 只是 Platform 编排或传输对象，不得进入 Kernel semantic ledger。

规范 Kernel contract 和可执行证据会明确记录各 projection 的状态。Node Agent 的有限历史转发已完成；持续观察由 Core gRPC stream 负责，因为 Node control envelope 是单命令单结果。Hardware 和 Sandbox Adapter 已完成各自本地事实/进程边界，不拥有 Event history。跨 host identity、systemd recovery 和特权部署验收仍是部署验证工作，不是 API 命名缺口。

## 3. 架构模型

`ExecutionAttachment` 是描述如何触达执行 authority 的 Platform orchestration contract，不是 Kernel semantic noun。

```text
Execution Agent
├── Host Agent       -> 现有 cy-node-agent -> 本地 KernelAuthority
└── Runtime Agent    -> 无特权 container 内的 cy-runtime-agent

Provider Adapter     -> Provider 所有的创建/停止/观测实现
```

冻结的逻辑 attachment 类型：

| Attachment 类型 | 含义 | 常见持久性 |
|---|---|---|
| `HOST_AGENT` | 持久运行的出站 Node Agent，将 control plane 桥接到一个节点本地 Kernel authority。 | `PERSISTENT` |
| `CONTAINER_AGENT` | Runtime Agent 在分配的 container 内运行、主动出站、暂存 Artifact，并在无 host 特权时监管一个 child workload。 | `EPHEMERAL` |
| `PROVIDER_MANAGED` | Provider adapter 创建/停止 workload 并提供 Provider observation；可在 workload 内附加 Runtime Agent。 | Provider 定义，通常 `EPHEMERAL` |

每个 Node 都带显式 `persistent: bool`，与 `node_type` 正交。旧 `PersistenceClass` wire field 仅作为兼容 projection 保留，准入时若与 boolean 不一致则拒绝。持久 Node 使用同一 `NodeId` 和新 `NodeSessionId`，经历 `ONLINE -> OFFLINE -> ONLINE`；临时 Node 可关闭为 `ONLINE -> LOST -> TERMINATED`，替代者必须使用新 `NodeId`。

示例：本地 WSL/Linux 和自有 GPU 工作站是 `HOST_AGENT + PERSISTENT`；Vast/RunPod Docker-only 租用节点是 `CONTAINER_AGENT + EPHEMERAL`；Kubernetes 是 `PROVIDER_MANAGED + EPHEMERAL`，可选择在 Pod 内附加 Runtime Agent。

`ExecutionAttachment` 不创建新的 Kernel entity。`HOST_AGENT` 复用 Node Agent；`CONTAINER_AGENT` 是容器内主动出站的无特权 Runtime Agent；`PROVIDER_MANAGED` 由 Provider Adapter 创建、停止和观测 workload。`persistent` 直接影响断线、Lease 过期和重连行为。Restart 必须显式声明：无人监管 container 使用 `NONE`；Host Agent 使用 `HOST_SUPERVISED`；Provider Adapter 使用 `PROVIDER_SUPERVISED`。契约不承诺已销毁的 container 能自行重启。

## 4. 身份模型

下列身份相互独立，不能相互推导：

| Identity | Owner 与生命周期 |
|---|---|
| `UserId` | Identity provider；与人类账号同寿命。 |
| `OrganizationId` | Account/authorization plane；与组织同寿命。 |
| `WorkspaceId` | Product/control-plane 作用域；与 Workspace 同寿命。 |
| `NodeId` | 稳定的已注册 host identity；对应现有 `NodeRef.node_id`。 |
| `NodeSessionId` | 一次认证的 Node Agent connection session；对应 `NodeWelcome.session_id`。 |
| `RuntimeId` | Control plane 选择的稳定逻辑 execution-runtime identity。 |
| `RuntimeGeneration` | `RuntimeId` 的非零 incarnation，遵循 semantic `Identity.generation` 规则。 |
| `OperationId` | 现有 Kernel semantic `Operation.identity`，表示一次有界转换。 |
| `AttemptId` | Product-neutral control-plane Attempt，可按 Product retry policy 跨越替换后的 Operation/Runtime generation。 |
| `WorkloadIdentity` | 只委派给一个 Runtime generation 的短期 scoped identity。 |

`ProductRun != Attempt != Kernel Operation != Runtime != container`。container ID 只是 Provider-private observation。销毁 container 不会删除 Product Run、Attempt history、Artifact 或 Runtime 逻辑 identity。替代进程/container 使用更高 Runtime generation；旧 generation 是不可变历史，不能重新获得 authority。

`RuntimeId` 和 `RuntimeGeneration` 在 wire 上复用冻结 semantic `Identity` 的 `id + generation` 规则，不创建第二套 identity engine。Package runtime 的 `RuntimeGeneration` 只管理 package binding 的本地 incarnation；映射关系由 control plane 记录，不得互相猜测。

## 5. Control 模型

### 5.1 唯一出站 Control Wire

v1 唯一 wire authority 仍是 `cyrene.core.v1.NodeControlService.Connect`，即 Protobuf/gRPC 双向 stream。Protocol v1 保留 `NodeHello` host projection；Protocol v2 在相同 envelope 中增加 `ExecutionAgentHello` 分支和通用 execution frame，不复用已有 field number。

每个 Agent 主动建立认证后的出站连接，无需入站 listener、VPN、Docker socket 或可发现的 container IP。v2 只扩展同一 envelope，不另建 `RuntimeControlService` 或第二套消息总线。

### 5.2 Session 与 Frame 规则

每个 frame 携带：幂等用的唯一 `frame_id`；严格递增的发送方 `sequence_number`；`ack_sequence_number`（已持久应用的对端最高连续序号）；welcome 后认证的 `session_id`；以及 hello/welcome 协商的 protocol 和 semantic compatibility。

`Hello` 包含 attachment kind、persistence class、稳定 runtime identity、agent version、支持的 protocol range、capability、可选完整 Provider inventory、resume token/cursor 和 bootstrap proof。`Welcome` 选择版本、建立新 session、返回 heartbeat/Lease policy 并确认 replay 状态。

Control vocabulary 支持 enrollment/session 建立、capability 广告/完整 inventory、heartbeat/Lease renewal request/result、assignment/幂等 `AssignmentAck`、desired state、Runtime/Worker/Operation observation、progress、结构化语义 Event、Provider observation/reconciliation hint、Artifact/log reference（不传 bulk Artifact bytes）、`StopCommand(command_id)` 与幂等 `StopAck(command_id)`，以及从最后连续确认序号重连和 replay。

Connection 是 transport state，不是 authority state。重连会创建新 session。只有 control plane 确认相同 enrolled identity 和活动 Runtime generation 后，resume cursor 才能恢复未确认 observation。`GAP` 或 authority generation 改变会触发 snapshot reconciliation，复用现有 Kernel replay 语义，而非另造 event-sourcing。

Alpha reference control service 会将首次 enrollment 保持 pending，直到收到首个经过认证的 Welcome 后 frame；若 Welcome 丢失，可重放同一 grant/token，不消耗第二个一次性 proof。Opaque token 在 grant 有效期内保持稳定。此 pending/grant state 目前只在内存中；control service 重启必须依赖生产级持久 session state 或重新 enrollment，不得伪造 fallback。

## 6. Lease、丢失与协调

规范 semantic `Lease` 和 fence token 是唯一 execution authority。Assignment 携带已签发的 Lease；Runtime Agent 可校验、续期和提交它，但不能创建、替换或重新解释 Lease。Host 模式从本地 `KernelAuthorityService` 获取。Container 与 Provider-managed deployment 必须绑定到唯一且符合规范的 execution authority implementation；MVP control fixture 使用 canonical `cy-kernel-contract` projection，并明确不是第二个生产 Lease store。

Agent liveness 根据四项判断：desired Runtime state、Agent observation、可用时的 Provider observation、以及包含 expiry/fence 的 Lease observation。单次 heartbeat 缺失不等于 container crash。

Reconciliation 将终态 observation 分类为：

- `EXPECTED_TERMINATION`：desired state 为 `STOPPED`，或已接受 stop 正在执行；之后 Lease 消失符合该意图；
- `GRACEFUL_TERMINATION`：child 正常处理 graceful termination，且已观测到 cleanup；
- `UNEXPECTED_LOSS`：desired Runtime 仍活动，但 Lease authority 丢失，且没有 Provider 解释或 Agent 最终 observation；
- `EXTERNAL_TERMINATION`：Provider 确认 stop、preemption 或 host 操作；
- `UNKNOWN_LOSS`：证据矛盾或不足。

若 Agent 和 Lease 同时消失，而独立 Provider 仍报告 `RUNNING`，reconciliation 应返回 network-partition candidate，而不是 crash 分类。

Runtime generation 和 Lease fence 均须校验。Stale generation、expired Lease、wrong holder 或 wrong fence，必须在修改 child 状态前拒绝。较新 observation 不能被重复或乱序 frame 覆盖。Event 是证据，reconciled authority state 才是真实状态。单次断线或 heartbeat 缺失不能直接视为 crash。


## 7. 优雅停止

正常关闭遵循以下冻结流程：

1. 控制面持久化 `DesiredRuntimeState = STOPPED`；
2. 发送 `StopCommand(command_id, runtime, grace_period)`；
3. Runtime Agent 持久化并去重该命令，然后返回 `StopAck(command_id)`；
4. Agent 向受监管的子进程组发送 `SIGTERM`，在有界排空/检查点窗口内等待，刷新观测 outbox，并且仅在期限到达后才强制终止；
5. 如果通道可用，Agent 上报最终观测。

如果最终的 `RuntimeStopped` 观测丢失，`Desired=STOPPED + StopAck + 后续 Lease 消失` 组合已足以将其归类为预期终止。缺少这些事实时，强制 kill 仍属于意外或未知，直到 Provider 证据能够判定。

## 8. Runtime 更新模型

`RuntimeProfile` 不可变，包含 image digest 与 environment digest。`latest` 等 tag、隐式执行 `pip install --upgrade` 或可变工作目录都不能作为 Runtime identity。

更新流程为“创建新 generation → readiness 检查 → 切换 → 排空旧 generation → 清理”。训练与推理可使用不同 profile。image/package 更新不得静默改写正在运行的 assignment。

## 9. Artifact 模型

Artifact Plane 与 control plane 分离。现有 `ArtifactRef` 保持 identity authority。`Node != ArtifactPeer`：Node 可以声明 Peer capability；object-store gateway 也可以是 Peer，但不是 Node。v1 增加以下传输记录：

- `ArtifactPeer`：带有 policy/health 信息的单个传输端点；
- `ArtifactReplica`：现有 Artifact identity 的一个 Provider-private 获取位置，包含区域、协议、优先级、有效期与 capability；
- `TransferPlan`：按分片路由来源；从第一版契约起就允许不同分片使用不同 Peer；
- `TransferTicket`：policy 选择之后签发的短期授权，范围限定在 Artifact、source、destination、chunk、expiry 与 byte；
- `TransferSession`：将一个 Artifact 传到一个目标位置的一次可恢复传输；
- `TransferPart`：有界字节范围及预期的分片 SHA-256；
- `TransferCheckpoint`：已持久完成的分片及 manifest/session identity。

Product 资源只包含 `artifact://sha256/...`、digest、manifest 和逻辑元数据。不得包含 `C:\...`、`/home/...`、`/mnt/...`、预签名 URL 或 Provider credential。Replica URL 与本地路径只存在于 Artifact Provider/transfer state 内。

Control stream 只传 Artifact identity、manifest/digest 与短期 scoped ticket；大型数据通过独立 Artifact Plane 传输。一个 Artifact 可以有多个 Replica，地址不是 identity。

Core v1 的 `RuntimeAssignment` 通过 `ArtifactTransferSpec` 表示远端输入，通过新增的 `ArtifactLocalInput` 表示已在本地的输入。`VerifiedLocal` 必须映射到精确、不可变的 identity projection；wire 不传本地路径、locator、transfer ticket 或 credential。workload 启动前，Runtime Agent 只能根据 digest 推导 CAS 条目，并重新读取完整文件以核对 SHA-256 和大小。内容缺失或不匹配时必须拒绝 Assignment，且不得留下运行中的子进程。此 projection 不会把 Artifact locality、Replica 或 ticket authority 转移到 Framework。

## 10. Artifact 传输 MVP

参考 transfer Provider 实现 HTTPS byte range、有界并发、分片 SHA-256 校验、持久 checkpoint、可恢复重试、完整 Artifact digest 校验，以及仅在验证完成后通过 rename/link 原子发布。部分下载的目标文件不得发布为 Artifact 路径。

`ArtifactSourceResolver -> TransferPlanner -> TransferPlan` 由单一 Artifact Plane coordinator 所有。它先依据 authorization、residency、trust domain、classification 和 policy 过滤来源，然后才考虑 health、latency、bandwidth 或 cost。MVP planner 将全部分片路由到一个选定的 seed；wire 和 Rust plan 则允许按分片使用多个 Peer。契约允许未来增加区域镜像、LAN peer、P2P、Dragonfly 或 cloud-native Provider。v1 不实现 global CDN、P2P discovery、类似 BitTorrent 的交换或 Dragonfly scheduler。

第一版只实现 HTTPS Range、有界多分片并发、分片与完整 digest、checkpoint resume 和原子发布。区域副本与 P2P 属于 Provider extension，不属于 Artifact identity。

## 11. 外部来源获取

Runtime 启动不得直接依赖 `git clone github.com` 或实时 Hugging Face 下载。长期流程为：

```text
ExternalSource -> AcquisitionProvider -> SourceImportJob
               -> SourceSnapshot -> internal Artifact -> Replica/cache
```

`ExternalSource` 是 Provider 专用的来源请求；`SourceSnapshot` 是不可变且绑定 digest 的结果；`SourceImportJob` 是通用 Operation 引用；`AcquisitionProvider` 是可替换 port。参考 HTTP Provider 会将 HTTP/HF 风格 fixture 流式写入临时文件，验证 digest，并以原子方式发布规范的内部 Artifact。它不声称提供中国/美国区域镜像。

Runtime 启动只消费内部 Artifact。GitHub/Hugging Face 等外部来源先由可替换的 AcquisitionProvider 导入并冻结为 SourceSnapshot，再进入 Artifact/Replica 流程。

## 12. 账号发现、注册与安全

架构允许采用“同一账号 → 发现 Organization → 发现 Workspace → 注册设备/Runtime → 建立连接”的流程，但 v1 不实现完整账号系统。

MVP 通过经过认证的 TLS 接收一次性 development enrollment token，并将其交换为短期 Workload identity。该 identity 限定于一个 Workspace、一个 Runtime identity/generation、获准的 capability、Artifact action 和有效期。原始 token 不得写入 journal 或返回到 Event 中。

契约为生产级 `EnrollmentProvider` 留出 seam，以支持 OIDC、OAuth Device Authorization、SSO 与企业审批。User identity、Node identity 和 Workload identity 彼此独立。container 不得收到长期 Organization credential、用户密码、cloud-admin credential 或 database-admin secret。Artifact 访问使用独立的短期 scoped credential。

开发 token 只是明确标记的 reference implementation，不代表生产身份方案。container 仅持有短期 Workload identity 和 scoped Artifact credential。

## 13. Connectivity Provider seam

`ConnectivityProvider` 提供经过认证的出站 control channel，并可选择提供直接 Artifact/Endpoint connectivity。Identity 不依赖 Tailscale IP、container IP、host IP、relay 地址或云私网地址。

契约定义的模式为 `LOCAL`、`LAN_DIRECT`、`DIRECT`、`OVERLAY` 和 `RELAY`。MVP 通过 TLS/gRPC 和普通 HTTPS 实现仅出站的 `LOCAL` Provider 及 relay-first 的 `RELAY` Provider。未来 Provider 可集成 Tailscale、原生直连或云私有网络。v1 不实现 WireGuard、STUN、ICE、NAT traversal 或全球 relay 网络。

## 14. 可靠性不变量

1. 命令和观测按稳定 ID 实现幂等。
2. 只有内容完全相同的重放才可接受重复投递。
3. 发送方序号单调递增；接收方 ack 是已持久应用的最高连续序号。
4. 乱序观测不得推进 authority。
5. Runtime generation 与 Lease fence 必须分别校验。
6. 重连会更换 session identity，不更换 Runtime identity。
7. Process/container 丢失必须根据 desired state、Agent、Provider 与 Lease 事实进行协调判定。
8. Event/log 是观测记录，不能充当唯一状态存储。
9. 每个逻辑 Runtime generation 只能有一个组件拥有 retry/restart policy。
10. Product-neutral wire field 不包含 TrainingRun、Dataset、ModelVersion、Deployment、Catalyst、Echo、Reactor、Exchange、Navigator 或 Yield 类型。

所有消息处理必须容忍 duplicate、reconnect、out-of-order、stale generation、process crash 与 network partition。最终状态来自 authority 与 reconciliation。

## 15. Ownership 矩阵

| 关注点 | 唯一 authority | 消费方 / projection | 不得拥有 |
|---|---|---|---|
| Lease/Fence | Kernel semantic authority（`cy-kernel-contract` / conforming `KernelAuthority`） | Host Agent、Runtime Agent 校验、Provider reconciler | Product、Runtime Agent、event store、Artifact service |
| Runtime identity/generation | 使用 semantic `Identity` 规则的 Fabric control-plane Runtime 记录 | Runtime Agent session、Provider Adapter | container ID、Node session、package catalog |
| Worker/Operation 生命周期 | Kernel semantic authority | Fabric observations 与 Product Attempt 映射 | package install state、connection state |
| Capability | semantic Capability 与现有 resolver/catalog authority | capability 广告快照 | Agent 私有临时 schema |
| Artifact identity | `cy-manifest` / Artifact schemas | Python/Rust SDK projection 和 transfer Provider | URL、path、TransferSession |
| Execution target placement | 基于调用方提供观测的 `cy-execution-fabric` 确定性 planner | control-plane assignment workflow | Resource admission、Lease 签发、Artifact Replica/ticket 选择 |
| Wire | `NodeControlService.Connect` + `node_control.proto` | Host 和 Runtime Agent | 第二个 Runtime 专用 service |
| Package/runtime 生命周期 | `cy-package-runtime` | Fabric Runtime 可托管一个 binding | Fabric control session、Product binding state |
| Product Run/Attempt | Product domain/state authority | control-plane execution projection | Kernel、Runtime Agent、container Provider |

`DUPLICATE_AUTHORITIES=0` 是发布门槛，不是文档目标。



## 16. 实现映射

### ExecutionAttachment

- **概念：** 在不改变 Kernel 名词的前提下，说明 control plane 如何触达执行端。
- **规范 authority：** Platform orchestration record；wire projection 使用 semantic Identity。
- **当前已有实现：** protocol-v1 Host 与 protocol-v2 Runtime attachment、identity、persistence、restart、observation 和 assignment projection。
- **所需新代码：** Alpha reference seam 不需要；生产级 identity 与 replicated recovery composition 仍然需要。
- **建议位置：** contracts/proto/cyrene/core/v1/node_control.proto；纯 admission/reconciliation 放在 framework/crates/cy-execution-fabric/。
- **位置理由：** 复用唯一现有 Agent stream，并让 policy 留在 Kernel 之外。
- **依赖：** semantic contract、现有 Node control envelope。
- **不得拥有：** Lease、Worker、Operation、Product Run、Provider registry。
- **MVP 实现：** enum、persistence class、Runtime/account scope reference、validation。
- **未来扩展：** attachment selection/scheduling policy。

### Runtime Agent

- **概念：** 在 container 内运行、无特权、主动建立出站连接并监管一个 child workload 的 Agent。
- **规范 authority：** Fabric control state 中的 Runtime assignment；child authority 仍受 Lease/Fence 约束。
- **当前已有实现：** agents/runtime/cy-runtime-agent 提供无特权的出站 mTLS Agent、规范 assignment/Lease admission、可恢复 Artifact staging、child-process supervision 与 observation delivery。其 grant-lifetime resume token 会在锁定、防 symlink、仅 owner 可访问的 state directory 中，以原子方式按精确 Runtime generation 和 NodeRef namespace 持久化。
- **所需新代码：** accepted-assignment/workload recovery 及受监管的 restart mode；当前 CONTAINER_AGENT 声明 RestartCapability::None。
- **建议位置：** agents/runtime/cy-runtime-agent/。
- **位置理由：** 与现有 Host/Node Agent 并列，而不是替代它。
- **依赖：** cy-proto、cy-kernel-contract、cy-execution-fabric、Artifact transfer SDK。
- **不得拥有：** host resource、Docker daemon、Product semantics、Lease 签发、package installation。
- **MVP 实现：** cy-runtime-agent run -- <command>、出站 TLS、child process group、observations、reconnect、stop。
- **未来扩展：** 更丰富的 checkpoint、Windows container process boundary、多个隔离的 child slot。

### Execution target placement

- **概念：** 根据当前、Product-neutral 的 resource、lifecycle、policy、cost 与 Artifact locality evidence，选择一个执行 Node。
- **规范 authority：** cy-execution-fabric 只负责确定性 placement 决策及其解释。Kernel 拥有 resource 和 Lease 签发；Artifact Plane 拥有 Replica、Peer、ticket 与 transfer plan 的选择。
- **当前实现：** cy-execution-fabric 0.2 提供 plan_execution_placement 和 place_execution_target。planner 使用规范 ProviderSnapshot、ResourceQuery、capability contract、ArtifactRef，以及由 policy scope 限定且有时效的 Artifact Plane quote。
- **位置理由：** 组合现有 authority，不把 cloud-vendor 行为迁入 Framework，也不让 observations 具有绑定效力。
- **不得拥有：** Provider adapter、Resource allocation、Lease/Fence、Artifact Replica/ticket/transfer plan、Product retry policy 或 global scheduler。
- **MVP 证据：** placement TCK 覆盖确定性排序、rejection reason code、真实 Provider resource class、过期 Provider generation、Artifact locality、HTTPS/capability enforcement、transfer cost/deadline、quote scope/expiry，以及重复 Node 拒绝。
- **当前限制：** ResourceMatchEvidence 是只读 snapshot evidence，不是 admission。由于规范 Kernel API 当前从一个 query 获取一个 Lease，一次 placement request 只携带一个 ResourceQuery。CPU + RAM + accelerator 的原子 bundle 需要扩展 Kernel contract；Framework 不得用分别获取的 Lease 模拟原子 bundle。
- **集成 seam：** cy-execution-control 经认证的 Host Agent session 路由所选 NodeRef，发送现有 typed semantic AcquireLease command 和完全相同的 placement query，只将 Kernel 返回的 Lease 绑定到 RuntimeAssignment，并等待对应 Runtime generation 的 AssignmentAck。controller 不得签发或拥有 Lease。由调用方注入的 durable intent ledger 只保存 immutable payload digest、transition state，以及只读的 Node/Lease identity/fence evidence；Resource ownership 完全属于 Kernel authority。command 已投递后若发生 timeout，结果分类为 UNKNOWN_REQUIRES_RECONCILIATION；不得更换幂等 key 后盲目重新 acquire。
- **集成证据：** execution_assignment_tck 使用生产版 NodeControl service state machine、真实 cy-node-agent session/bridge、经 peer-credential 认证 UDS 提供的真实 Kernel authority service，以及真实 Runtime assignment admission validator。它证明：选中一个 Node、取得一个规范 Lease/fence、接受一个 assignment、抑制重复 dispatch、规范 release/resource reuse，以及通过同一 Node epoch 上的替代 Host session 精确幂等回滚 Lease。其 Runtime peer 是 protocol harness。另一项 mtls_runtime_agent_tck 使用本地签发的 CA/server/Host/Runtime certificate、生产版 fingerprint authenticator、真实 cy-node-agent、经 peer-credential 认证的 Kernel UDS、LinuxSystemProvider、真实 cy-runtime-agent 和 child process、重新打开的 file intent ledger，以及使用已持久 grant-lifetime resume token 的全新 Runtime Agent task/state invocation。Agent 与 control service 是组合在同一测试 OS process 中的真实生产实现；这并不能证明 Agent OS process restart、Docker 或多 container deployment。该 TCK 不会重启 ExecutionControlService；其中的 resume grant/token 与 accepted assignment binding 仍在内存中。
- **0.2 migration：** 调用方必须构造规范 Provider/resource snapshot 和 Artifact placement quote，不再传入临时 target score。公开的 request/candidate/result struct 有意构成从 0.1 到 0.2 的 breaking change。

执行目标 placement 只对调用方提供的当前观测做确定性决策与解释。ProviderSnapshot 匹配不是资源预留；只有 KernelAuthority 能签发 Lease。Artifact quote 必须由 Artifact Plane 基于同一 policy scope 生成，Framework 不接触 replica、ticket 或 TransferPlan。当前单个 ResourceQuery 与单 Lease API 对齐；CPU、内存、加速器的原子组合资源需要先扩展 Kernel Contract，不能在 Framework 内用多个独立 Lease 假装原子性。

### Host Agent seam

- **概念：** 到本地 Kernel 的持久 host bridge。
- **规范 authority：** 现有 cy-node-agent 与本地 KernelAuthorityService。
- **当前已有实现：** agents/node/cy-node-agent/。
- **所需新代码：** MVP 不需要；之后可增加 protocol-v2 adapter。
- **建议位置：** 现有 agents/node/cy-node-agent/。
- **位置理由：** 它已经拥有出站 mTLS、Node session fencing 和 typed Kernel forwarding。
- **依赖：** 本地 Kernel UDS、Node control wire。
- **不得拥有：** Kernel Lease state、hardware adapter 内部实现、container-only 模式下 Runtime child process。
- **MVP 实现：** contract mapping 与 conformance fixture。
- **未来扩展：** 在 protocol v2 上发送通用 Execution Agent frame。

### Provider-managed seam

- **概念：** Provider 创建/停止 workload，并提供独立 observations。
- **规范 authority：** semantic Provider/ProviderSnapshot 加 Provider Adapter 的私有 handle。
- **当前已有实现：** 本地 KernelProviderService projection。
- **所需新代码：** 需要，但仅是 reference seam。
- **建议位置：** 复用 contracts/proto/cyrene/provider/v1/kernel_provider.proto；Fabric reconciliation 放在 framework/crates/cy-execution-fabric/。
- **位置理由：** 复用已有且受 Provider generation fencing 的 Provider observation semantics。
- **依赖：** ProviderSnapshot、Runtime assignment、Lease observation。
- **不得拥有：** Product state 或 Kernel Lease mutation。
- **MVP 实现：** acceptance test 中的 FakeProvider observation。
- **未来扩展：** RunPod、Vast、Kubernetes、Slurm、Azure、GCP adapter。

### Control Wire

- **概念：** 经认证的 session、command、observation、ack/replay。
- **规范 authority：** NodeControlService.Connect 与 node_control.proto。
- **当前已有实现：** Rust cy-execution-control 拥有 protocol-v1 Host 和 protocol-v2 Runtime session state machine。原先仅用于 registration 的 JVM inbound service 及其 activeNodes registry 已退役，因此只有一个源码实现 owner。
- **所需新代码：** 生产环境 external identity/enrollment 的 composition 与具备多副本持久化能力的 session state；不需要第二个 NodeControl 实现。
- **建议位置：** contracts/proto/cyrene/core/v1/node_control.proto；由 contracts/rust/cy-proto 生成。
- **位置理由：** 避免形成第二个 wire authority。
- **依赖：** semantic contract 和 Artifact identity projection。
- **不得拥有：** bulk data、Event 中的 credential、仅以 Event 保存的 authority snapshot。
- **MVP 实现：** Execution Agent hello/welcome、assignment、ack、observation、renew、stop、sequence/ack。
- **未来扩展：** compression 与其他可选 observation payload。

### Enrollment

- **概念：** 引导流程，用于取得短期 Workload identity。
- **规范 authority：** 未来由 identity provider 提供；MVP 使用 development verifier。
- **当前已有实现：** 仅 Node mTLS file identity。
- **所需新代码：** 需要。
- **建议位置：** proof 放入 node_control.proto；verifier port 放在 cy-execution-fabric；fixture verifier 放在 acceptance support。
- **位置理由：** Authentication 保持在 transport/control boundary。
- **依赖：** TLS peer identity、account scope、Runtime generation。
- **不得拥有：** 用户密码或长期 Organization OAuth credential。
- **MVP 实现：** 一次性 development token、脱敏持久化、有 expiry 的 Workload identity。
- **未来扩展：** OIDC、OAuth Device Authorization、SSO、企业审批。

### Lease/fence 集成

- **概念：** 有时限的 execution authority，以及拒绝 stale mutation。
- **规范 authority：** 冻结的 Kernel Semantic Contract v1。
- **当前已有实现：** Kernel authority、resource manager、Node bridge。
- **所需新代码：** 需要；仅消费/校验，不新增 authority。
- **建议位置：** cy-execution-fabric validation 和 Runtime Agent assignment admission。
- **位置理由：** 不需要新增 Lease type/store。
- **依赖：** semantic Lease、Worker holder、expiry clock、fence。
- **不得拥有：** Runtime Agent 内部的 Lease 签发或替换。
- **MVP 实现：** renew request/result、expiry transition、stale generation/fence 拒绝。
- **未来扩展：** 生产级远程 authority routing 与 HA storage。

### Runtime state observation

- **概念：** 对 desired/observed Runtime 和终止证据进行对账。
- **规范 authority：** 与 semantic Worker/Operation/Lease 协调后的 Fabric Runtime record。
- **当前已有实现：** Product Attempt observation 与节点本地 Worker state 分别存在。
- **所需新代码：** 需要。
- **建议位置：** protocol-v2 observation frame 与 cy-execution-fabric reconciler。
- **位置理由：** state composition 属于 Kernel 之上、Product policy 之下。
- **依赖：** Agent、Provider、Lease、desired state。
- **不得拥有：** Product terminal semantics 或第二份 Event log。
- **MVP 实现：** state/progress/reason、termination classification、FakeProvider reconciliation。
- **未来扩展：** 特定 policy 的 remediation 和 scheduler feedback。

### Runtime generation

- **概念：** 不可变的 Runtime incarnation 与 update fencing。
- **规范 authority：** 使用 semantic Identity generation 的 Fabric Runtime record。
- **当前已有实现：** semantic generation 规则和 package-binding RuntimeGeneration。
- **所需新代码：** 需要 mapping 与 validation；不需要新的 identity engine。
- **建议位置：** wire 的 RuntimeRef/RuntimeProfile、cy-execution-fabric。
- **位置理由：** 保持 Runtime 位于 Kernel 之上，不让 container identity 承担过多语义。
- **依赖：** image/environment digest、assignment、Lease。
- **不得拥有：** package install identity 或 container ID。
- **MVP 实现：** 拒绝 zero/stale generation；证明替代 generation 能 fence 旧 Agent。
- **未来扩展：** readiness switch、drain、rollback coordination。

### 优雅停止

- **概念：** desired stop、幂等 command/ack、有界的 child termination。
- **规范 authority：** 持久化的 desired Runtime state 与 command state；Worker/Lease 的最终 authority 仍属于 Kernel semantic。
- **当前已有实现：** 节点本地 ServiceSupervisor 与 Worker stop path。
- **所需新代码：** 需要。
- **建议位置：** wire message、cy-execution-fabric command state、cy-runtime-agent process supervisor。
- **位置理由：** 将远程意图与本地 signal/cleanup 机制分开。
- **依赖：** process group、grace deadline、outbox flush。
- **不得拥有：** Product cancellation policy。
- **MVP 实现：** SIGTERM、期限后升级为强制终止、StopAck/最终 observation。
- **未来扩展：** workload checkpoint hook。

### Reconnect/replay

- **概念：** 恢复 control channel，但不复活 stale authority。
- **规范 authority：** session sequence/ack 加上现有 Kernel cursor semantics。
- **当前已有实现：** Node session sequence 和 opaque resume token；Agent 尚无 durable outbox。
- **所需新代码：** 需要。
- **建议位置：** protocol-v2 envelope field、cy-runtime-agent 有界 outbox/checkpoint、Fabric session state。
- **位置理由：** 扩展当前 channel，而不引入 event sourcing。
- **依赖：** enrolled identity、Runtime generation、ack cursor。
- **不得拥有：** Runtime 或 Lease identity。
- **MVP 实现：** 强制断开 stream、重连并证明 replay/dedupe。
- **未来扩展：** replicated session store 与有界 history compaction。

### Artifact transfer

- **概念：** Provider-neutral 地传输现有 Artifact identity，并支持恢复。
- **规范 authority：** 现有 ArtifactRef contract。
- **当前已有实现：** cy-artifact-transfer 提供规范 Rust projection、受 policy scope 限制的 source/Peer planner、scoped ticket boundary、HTTPS Range transfer、有界并发、分片与完整 digest 校验、durable checkpoint、resume 和原子发布。Runtime Agent 使用该实现，要求 Artifact Plane source projection，并在任何网络传输前调用 ticket verifier。打包提供的对称 verifier 明确只用于 development 和 conformance；生产环境必须注入受管 verification authority，且不得赋予 Agent 签发 ticket 的权限。
- **所需新代码：** 生产级 Artifact directory/ticket issuer。规范 local-input projection 已实现，且只包含 identity；workload 启动前，Runtime Agent 会重新验证 VerifiedLocal 内容。
- **建议位置：** sdk/rust/cy-artifact-transfer/；Runtime Agent 消费该 SDK；schema 仍位于 contracts/schemas/。
- **位置理由：** Transfer 是可复用的 SDK/data-plane concern，不是 Kernel 或 Product logic。
- **依赖：** HTTPS client、SHA-256、ArtifactRef、原子文件系统操作。
- **不得拥有：** Artifact identity、Product path、global Replica scheduling。
- **MVP 实现：** Range、有界并发、分片/完整校验、checkpoint、resume、原子发布。
- **Placement projection：** Artifact Plane 可派生保守且受 authorization 限制的 TransferEstimate，并只将中性的 scalar metric 投影到 ArtifactPlacementQuote；cy-execution-fabric 不依赖 transfer SDK，也不能选择 Replica 或签发 ticket。
- **未来扩展：** 区域、LAN、P2P、Dragonfly、cloud-native Provider。

### AcquisitionProvider

- **概念：** 将外部可变来源转换为内部不可变 Artifact。
- **规范 authority：** acquisition port；导入之后由 Artifact identity 负责。
- **当前已有实现：** cy-artifact-transfer::AcquisitionProvider 和 reference HTTP importer；后者发布经过 digest 校验的不可变 SourceSnapshot。
- **所需新代码：** 仅需生产 Git/Hugging Face 和区域 acquisition adapter；它们必须位于 Runtime 启动路径之外。
- **建议位置：** sdk/rust/cy-artifact-transfer/src/acquisition.rs。
- **位置理由：** acquisition 为 Artifact Plane 提供内容，并与 Runtime 启动解耦。
- **依赖：** Artifact Provider 与通用 Operation reference。
- **不得拥有：** Runtime launch、Product dataset/model semantics、外部 credential 持久化。
- **MVP 实现：** typed seam 和 fake snapshot proof。
- **未来扩展：** GitHub/Hugging Face/镜像 importer。

### ConnectivityProvider

- **概念：** 可替换的出站连接建立机制。
- **规范 authority：** Fabric connectivity port；identity 与连接方式无关。
- **当前已有实现：** Node Agent 中直接使用 tonic TLS。
- **所需新代码：** 需要 seam 和 direct Provider。
- **建议位置：** framework/crates/cy-execution-fabric/src/connectivity.rs；由 Runtime Agent client 使用。
- **位置理由：** Connectivity policy 属于 Platform orchestration，不属于 Kernel 或 Product state。
- **依赖：** 经过认证的 transport configuration。
- **不得拥有：** Node/Runtime identity 或 Lease state。
- **MVP 实现：** 直接出站 TLS/gRPC control 与 HTTPS Artifact path。
- **未来扩展：** 仅 relay、Tailscale、原生直连、cloud private network。



## 17. MVP 参考纵向切片与验收

本文固定的实现顺序如下：

1. 增量 wire projection 与 contract test；
2. 纯 Fabric admission/reconciliation library；
3. 监管一个 child 的无特权 Runtime Agent；
4. 支持恢复的 HTTPS Artifact transfer SDK；
5. reference control/provider/artifact fixture；
6. 在 Docker 中进行真实验收，覆盖 enrollment、capability 广告、运行状态观测、heartbeat/renewal、reconnect、优雅停止、docker stop、docker kill、Lease expiry、generation fencing、stale 拒绝，以及中断传输的恢复；每个分片和完整 Artifact 均须通过 SHA-256 校验后再原子发布。

Docker fixture 使用 development enrollment token、本地签发的测试 certificate，以及 development Artifact ticket authority。其 HTTPS source 只接受 Runtime Agent 携带的 signed ticket。这些证据证明 fixture 的 transport 与 authorization 流程，不证明生产环境 certificate 与 Agent identity 的绑定，也不证明生产 ticket issuer。当 Docker 可用时，不得跳过 Docker fault injection，并且必须报告精确的 pass/fail/skip 数量。Unit/fake test 不能替代真实 Docker 验收。

进程内运行的 mtls_runtime_agent_tck 是额外的可执行边界证明，覆盖 fingerprint-bound mTLS identity、Host 到 Kernel 的 UDS、真实 Runtime Agent workload launch、全新的 Agent task/state invocation、durable intent reopen 和规范 release。它补充但不替代 Docker fault matrix，也不能声称验证了 OS process restart。

本文冻结后的实现顺序不会在编码过程中重新分配 ownership。FakeProvider 只证明 Provider/Agent/Lease observation 可以组合；真实 Docker kill、reconnect、Lease expiry 和 Artifact resume 才构成最终的 container 模式证据。

## 18. 延后能力

### 后续必需项

- 生产级 OIDC/device enrollment 与 Workload identity issuer；
- 生产级、持久化的 Fabric session/runtime store、resume grant/token state、accepted assignment binding 和 HA reconciliation；当前 Agent token 能跨越全新的 Agent invocation 保留，但进程内 TCK 未能证明 OS process restart 或 control-service restart；
- 要声称 Agent process loss 后可恢复 workload，必须持久化 Runtime 已接受的 assignment/workload，并定义非 None 的 restart contract；
- 为未知的 Acquire/Assignment/Release 结果提供规范 reconciliation reader；durable intent ledger 已能避免盲目重新 dispatch，但只有 Kernel Event 与 Runtime observation 能解决未知结果；
- 在联合预留 CPU、RAM 和 accelerator 之前，先定义规范的原子 resource-bundle contract；

### 可选的规模扩展

- 区域 Artifact Replica 选择与镜像；
- LAN peer、P2P 和 Dragonfly 风格的 transfer Provider；
- 多 Runtime placement 和 global scheduler policy。

### Provider 专属扩展

- RunPod、Vast、Kubernetes、Slurm、Azure 和 GCP adapter；
- 面向更多操作系统的持久 host Node Agent packaging。

### 网络基础设施

- relay fleet、Tailscale Provider、原生直连能力；
- 只有后续证据证明有必要时才增加 STUN/ICE/NAT traversal。

只要冻结的 seam、reference vertical 和 fault-injection proof 通过，上述延后项就不会导致 v1 验收失败。
