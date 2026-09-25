# CYRENE Kernel Semantic Contract v1

Status: **Frozen v1.0** (2026-08-11).

This document is the sole normative authority for Kernel semantics. The
Protobuf schema, Rust model, C ABI shim, JVM/Python SDKs and transports are
projections. The language-neutral TCK is the executable acceptance projection
of this document. A conflict is a defect in the projection, never an implicit
change to this contract.

## 1. Purpose and boundary

The Kernel is a node-local authority for identity, ownership, allocation,
fencing, bounded lifecycle transitions and ordered observations. It is not an
AI framework, package manager, hardware driver, container engine, scheduler,
network proxy or business service. A conforming implementation may be
rewritten in any memory-safe language without preserving Rust types or module
layout.

The nine durable nouns are:

1. `Principal` — authenticated actor derived by a transport boundary.
2. `Provider` — authenticated out-of-process source of facts or execution.
3. `Resource` — Provider-owned allocatable unit described generically.
4. `Lease` — exclusive, finite and fenced authority over Resources.
5. `Worker` — execution identity in an external failure domain.
6. `Operation` — bounded, cancellable lifecycle transition.
7. `Capability` — namespaced, revisioned exact-match fact.
8. `Endpoint` — authorization metadata for a direct data path.
9. `Event` — immutable ordered observation, never a callback.

`Identity`, `ContractRevision`, `ResourceQuery`, `Quantity`, `EndpointGrant`,
`EventCursor`, `EventPage`, `EventContinuity` and `Rejection` are bounded value
objects. Product,
model, dataset, runtime-language, container and hardware-vendor vocabulary is
outside Kernel semantics.

## 2. Revision and projection rules

The semantic revision is `{ contract_id = "cyrene.kernel.semantic", major = 1,
minor = 0 }`. It is negotiated before state-changing actions and independently
from gRPC, UDS framing, Protobuf package, Worker protocol or Adapter protocol
versions. Revisions are compatible only when contract ID and major are equal;
the selected minor is the lower supported minor. Incompatibility fails closed
with `CONTRACT_INCOMPATIBLE`.

Every projection must:

- authenticate a `Principal` outside caller-controlled message bodies;
- reject missing required message fields, `UNSPECIFIED` states and unknown
  enum numbers rather than inventing a default;
- preserve IDs, generations, fence tokens, quantities and timestamps exactly;
- preserve stable `reason_code` values even when mapping them to local status
  mechanisms;
- ignore unknown optional fields only when doing so cannot weaken a v1
  authority or lifecycle decision;
- keep native handles, pointers, PIDs, file descriptors, device handles,
  credentials and implementation paths outside the semantic wire contract.

A C ABI is a client/projection boundary, not an in-process plugin ABI. Vendor
libraries and untrusted extensions stay in external processes connected by a
bounded authenticated transport such as UDS.

## 3. Common validity and bounds

An opaque identity ID is non-empty UTF-8 without control characters and at
most 256 bytes. `generation` is non-zero. Identity equality always compares
both fields. The field that contains an Identity supplies its type: for
example `Resource.provider` references a Provider and `Worker.lease`
references a Lease; IDs need only be unique within their noun.

A namespaced identifier is at most 128 ASCII bytes. It consists of segments
matching `[a-z][a-z0-9]*`, separated by exactly one `.`, `-` or `_`. Empty
segments, uppercase letters and leading/trailing separators are invalid.
Provider observation reason identifiers follow that grammar. A semantic
`Rejection.reason_code` instead uses upper snake case
`[A-Z][A-Z0-9_]*`, also bounded to 128 bytes.

Timestamps are exact Unix milliseconds in `1..=253402300799999`. A Protobuf
`Timestamp` projection rejects out-of-range values and nanoseconds not exactly
representable as milliseconds. Deadlines may be absent where the noun says so;
Lease, EndpointGrant and ProviderSnapshot expiry are mandatory.

| Bound | Frozen v1 value |
|---|---:|
| Identity ID | 256 bytes |
| Namespaced ID / reason code | 128 bytes |
| Capabilities per object | 64 |
| Properties, capacity entries, topology links or Worker limits | 64 |
| Resources per ProviderSnapshot | 1,024 |
| Resources per Lease / ResourceQuery count | 256 |
| Workers or Endpoints per ProviderSnapshot | 4,096 each |
| Worker execution reference | 512 bytes |
| Rejection message | 1,024 bytes |
| Event body | 65,536 bytes |
| Events per page | 256 |

Maps use namespaced keys and non-empty values of at most 256 UTF-8 bytes
without control characters. Repeated capabilities and requirements cannot
contain duplicate capability IDs. A complete snapshot cannot contain two
incarnations with the same stable Resource, Worker or Endpoint ID.

## 4. Identity, generation and fencing

Generations describe incarnation, not ordinary mutable state:

- a `Provider` generation changes for every new authenticated Provider
  session/process incarnation;
- a `Resource` generation changes only when the physical/logical allocatable
  unit is replaced or continuity cannot be proved; capacity, temperature,
  free memory and health changes do not change it;
- a `ProviderSnapshot.snapshot_generation` is a separate sequence, scoped to
  one Provider generation, and strictly increases for every accepted complete
  publication;
- Worker, Lease, Operation and Endpoint IDs may be reused only with a higher
  generation after termination/replacement.

Provider restart never resets an old Provider generation. Resources may keep
their generation only when continuity is independently proved; otherwise they
advance. Snapshot generation must never be copied into Resource generation.

Fence tokens are non-zero, monotonically allocated by Kernel and never reused
for the same node authority, including after restart. The next range is durably
reserved before a Lease becomes externally visible; failure to preserve that
property is a startup/allocation failure. Generation and fence checks are both
required—neither substitutes for the other.

## 5. Principal, Provider, Resource and Capability

The transport authenticates a Principal from UDS peer credentials, mTLS or an
equivalent trusted mechanism and injects it into `KernelAuthority`. A request
field cannot self-assert Principal. Authorization policy may decide which
Worker IDs a Principal owns, but it cannot weaken generation or fence checks.

A Provider registers one authenticated incarnation before publishing facts.
Its `READY`, `DEGRADED` and `UNAVAILABLE` state and capabilities are
observations. Provider failure is isolated per Provider and cannot make an
unrelated Provider unavailable.

Capability matching supports only:

- exact capability ID equality;
- `provided.revision >= required.minimum_revision`;
- exact equality of explicitly required string properties;
- unsigned scalar comparison with exactly equal units.

There is no expression language, script, callback, regex engine or
Provider-supplied code in Kernel. Rich scheduling is compiled externally into
the bounded `ResourceQuery`.

A Resource class is opaque, such as `accelerator` or `execution.slot`. Kernel
does not interpret class, capability, capacity or attribute names. Lease v1 is
exclusive only. A Provider expresses safe sharing by publishing independently
fenceable partition/slot Resources; it never asks Kernel to infer sharing from
vendor topology. A v1 query atomically selects one class. Heterogeneous policy
is resolved outside Kernel into ordered acquisitions or a Provider-published
composite Resource.

CPU/memory values that merely cap one Worker are generic Worker `limits`, not
additional Lease grants. CPU/memory becomes allocatable authority only if a
Provider explicitly publishes corresponding Resources.

## 6. Lease authority and lifecycle

A Lease contains a non-empty Resource set, one planned Worker holder, an
`ACTIVE`-origin lifecycle, a non-zero fence and a finite expiry. It authorizes
an action only when all are true:

- the Lease is valid and `ACTIVE`;
- holder Identity and generation match;
- Resource Identity and generation are in the Lease;
- presented fence equals the Lease fence;
- current time is strictly before expiry.

Expiry removes authority immediately even before an `EXPIRED` event is
observed. Renewal requires current identity/fence, cannot shorten expiry, and
preserves identity, resources and fence. Replacement/revocation advances
authority so stale Workers cannot reconnect.

Legal Lease transitions (self-transition is idempotent replay):

| From | Additional legal targets |
|---|---|
| `ACTIVE` | `RELEASING`, `EXPIRED`, `REVOKED`, `FAILED` |
| `RELEASING` | `RELEASED`, `REVOKED`, `FAILED` |
| `RELEASED`, `EXPIRED`, `REVOKED`, `FAILED` | none; terminal |

Release is therefore `ACTIVE → RELEASING → RELEASED`; an implementation may
hide the intermediate observation but may not apply a direct state mutation.

## 7. Worker and Operation lifecycle

A Worker references exactly one Principal, Provider and Lease and carries an
immutable externally verified execution reference. Its generic limits are
enforced by an external execution/sandbox Provider. Kernel stores no command,
environment, native process handle or vendor runtime object in Worker.

Legal Worker transitions:

| From | Additional legal targets |
|---|---|
| `REGISTERED` | `STARTING`, `DRAINING`, `STOPPED`, `FAILED`, `LOST` |
| `STARTING` | `RUNNING`, `DRAINING`, `STOPPED`, `FAILED`, `LOST` |
| `RUNNING` | `DRAINING`, `STOPPED`, `FAILED`, `LOST` |
| `DRAINING` | `STOPPED`, `FAILED`, `LOST` |
| `STOPPED`, `FAILED`, `LOST` | none; terminal |

An Operation has one Principal owner, one Provider/Worker executor, an opaque
namespaced kind, optional deadline, optional parent Operation and bounded
metadata. Its lifecycle is:

| From | Additional legal targets |
|---|---|
| `CREATED` | `PENDING`, `RUNNING`, `CANCELLING`, `CANCELLED`, `FAILED` |
| `PENDING` | `RUNNING`, `CANCELLING`, `CANCELLED`, `FAILED`, `LOST` |
| `RUNNING` | `SUCCEEDED`, `FAILED`, `CANCELLING`, `LOST` |
| `CANCELLING` | `CANCELLED`, `FAILED`, `LOST` |
| `SUCCEEDED`, `FAILED`, `CANCELLED`, `LOST` | none; terminal |

Self-transition is an idempotent replay. Any other transition is rejected with
`STATE_TRANSITION_INVALID`; restart requires a higher object generation.

## 8. Endpoint authorization

Endpoint contains metadata only; its data never traverses Kernel. A Provider
may publish an Endpoint only for an existing Worker owner, under a Principal
authorized for that Worker. Only that owner Principal or Kernel revocation
policy may authorize/revoke grants.

An EndpointGrant is valid only while all of these hold:

- grant, Endpoint, grantee, Lease identities and generations match;
- the grantee is the referenced Lease holder;
- grant fence equals the active Lease fence;
- both Lease and grant are strictly before expiry.

Credentials are delivered out of band by the Endpoint Provider and never
appear in Endpoint public attributes or Events.

## 9. Event ordering and replay

Every Event carries the Kernel/node authority `source` Identity. Sequence is
strictly increasing within that source generation; the resume cursor is the
pair `(source, sequence)`. Event body is optional descriptive payload under a
registered schema ID. It never carries authority, credentials, native handles
or state required to interpret the Event header.

`ReadEvents` returns an `EventPage` of at most 256 events:

- `CURRENT`: same source and retained history covers `sequence + 1`; events
  are strictly ordered and `next_sequence` equals the last returned sequence;
- `GAP`: source matches but requested history was evicted; no incremental
  events are returned and a fresh snapshot/reconcile is required;
- `SOURCE_CHANGED`: Kernel authority incarnation differs; no incremental
  events are returned and a fresh snapshot/reconcile is required.

An empty current page preserves the input cursor. Gap/source change must never
be silently converted into the oldest retained event.

When the selected event store supplies durable history, it is canonical for
same-source replay and the authority snapshot cursor; an in-memory event cache
must not shorten that history. Durable read errors or corruption fail closed
rather than falling back to cached events. After `GAP` or `SOURCE_CHANGED`, a
client rebuilds from the existing authority snapshot and resumes with its
returned cursor. Events from an earlier source generation are audit history and
cannot rebuild active authority state for a new Kernel epoch.

`WatchEvents` begins with the same cursor continuity decision, replays retained
events as a server stream, and then observes the live authority. Each intact
event is emitted as an `Event`. A `GAP` or `SOURCE_CHANGED` is emitted as a
typed `EventContinuity` stream frame and closes the stream; it is not converted
into a generic transport status. `EventPage` remains exclusive to the finite
`ReadEvents` result. `ReadEvents` and `WatchEvents` share the same cursor,
source identity and durable history rules.

## 10. Provider snapshot and reconciliation

A ProviderSnapshot is complete, expiring and scoped to exactly one registered
Provider generation. Every included Resource, Worker and Endpoint validates
and references that Provider. Missing/expired facts become unavailable for new
authority; they never override Kernel-owned Lease, fence or Operation state.

Reconciliation is fail-closed and per Provider:

- stale Provider or snapshot generation is rejected;
- expired/missing Resources become `UNAVAILABLE` for new Leases;
- an existing Lease remains its current lifecycle state but no longer gains
  new authority from unavailable facts; Kernel emits a degraded/unavailable
  Resource/Worker Event and explicit policy may move the Lease to
  `RELEASING`, `REVOKED` or `FAILED`;
- missing Workers become `LOST`, after which their Leases are revoked;
- Kernel never adopts an unproved process, Resource owner or old fence.

There is deliberately no `DEGRADED` Lease state: health is an observation,
whereas Lease state is authority lifecycle.

## 11. Stable actions and rejections

The v1 actions are `NEGOTIATE`, `REGISTER_PROVIDER`, `PUBLISH_INVENTORY`,
`RECONCILE_PROVIDER`, `ACQUIRE_LEASE`, `RENEW_LEASE`, `RELEASE_LEASE`,
`START_WORKER`, `HEARTBEAT_WORKER`, `STOP_WORKER`, `CREATE_OPERATION`,
`REPORT_OPERATION`, `CANCEL_OPERATION`, `PUBLISH_ENDPOINT`,
`AUTHORIZE_ENDPOINT`, `REVOKE_ENDPOINT`, `READ_EVENTS` and `WATCH_EVENTS`. A
transport may combine entry points but cannot omit their validation or authority
semantics.

Stable action-level reason codes are:

- compatibility/authentication: `CONTRACT_INCOMPATIBLE`,
  `AUTHENTICATION_REQUIRED`, `AUTHORITY_DENIED`;
- projection validity: `REQUIRED_FIELD_MISSING`, `UNKNOWN_ENUM_VALUE`,
  `TEXT_INVALID`, `NAMESPACED_ID_INVALID`, `REASON_CODE_INVALID`,
  `TIMESTAMP_INVALID`;
- authority: `GENERATION_INVALID`, `STALE_GENERATION`,
  `FENCE_TOKEN_INVALID`, `FENCE_MISMATCH`, `LEASE_EXPIRED`,
  `LEASE_NOT_ACTIVE`, `LEASE_RENEWAL_INVALID`;
- lifecycle/replay: `STATE_TRANSITION_INVALID`, `REPLAY_GAP`,
  `EVENT_SOURCE_CHANGED`.

Object-specific validation codes in the Rust reference projection and TCK are
also stable for v1. Human messages are bounded diagnostics and are not stable
program logic. A projection may map a reason code to gRPC/errno/exception
status, but the reason code remains available unchanged.

## 12. Compatibility, conformance and evolution

Existing `cyrene.core.v1` Plugin-named APIs are a temporary compatibility
projection. They are not semantic authority and must not be used to justify
weaker Principal, TTL, generation, fence, transition or replay behavior. New
public APIs use the semantic nouns and `KernelAuthority`; compatibility
translation stays at the outer transport edge.

At this freeze, the specification, semantic Proto, pure Rust model and
Kernel-semantic TCK define v1. The current API/semantic projection status is
explicitly recorded below; deployment hardening is tracked separately and does
not downgrade a completed naming or contract projection.

| Component | Current status | Evidence and exact scope |
| --- | --- | --- |
| Core gRPC | **COMPLETE** | v1/v2 expose finite `ReadEvents` and continuous `WatchEvents`; `ReadEvents` returns `EventPage`, while `WatchEvents` emits typed `EventContinuity` frames for `GAP` and `SOURCE_CHANGED`; daemon RPC tests cover replay, handoff, backpressure and reconnect. |
| Node Agent | **COMPLETE** | The one-command/one-result Node control projection carries finite `ReadEvents` and `EventPage` results; continuous `WatchEvents` is **NOT_APPLICABLE** to that envelope and remains on the canonical Core gRPC stream. Node bridge and integration tests pass. |
| Hardware Adapter | **NOT_APPLICABLE** for Event read/watch | The local adapter contract reports inventory facts and creates resource bindings; it does not own durable Event history or observation. Hardware adapter protocol round-trip and daemon integration tests pass. |
| Sandbox Adapter | **NOT_APPLICABLE** for Event read/watch | The local adapter contract launches, stops, observes and recovers concrete processes; it does not own durable Event history or cursor continuity. Sandbox protocol and UDS integration tests pass. |
| Rust semantic authority | **COMPLETE** | `Event`, `EventCursor`, `EventPage`, `EventContinuity` and the shared `ReplayStatus` define one semantic model; `KernelAuthority::read_events` is the finite history operation used by both projections. Rust semantic TCK and workspace tests pass. |
| Python consumer / TCK | **COMPLETE** | The checked-in Python Kernel Semantic TCK passes; Platform owns no second Python Event authority or alternate operation name. |
| Kotlin TCK / consumer | **COMPLETE** | The checked-in Kotlin Kernel Semantic TCK passes against the same frozen vectors and semantic names. |

The remaining `PRODUCTION_HARDENING / DEPLOYMENT_VALIDATION` items are
cross-host mTLS, systemd crash recovery, privileged deployment identity and
multi-account Linux acceptance. They are not missing API operations,
projections, or naming rules.

`KernelAuthorityService` is the canonical Core gRPC projection. Every
state-changing authority action requires a selected `ContractRevision` in an
`AuthorityCallContext`; it carries no caller-supplied Principal, `NodeRef`,
plugin installation or vendor-specific data. `WorkerControlService` is a
separate, restricted bidirectional liveness/drain projection: its hello frame
contains only the already-authorized Worker and Lease identities plus a fence,
and Kernel returns the authoritative Worker record. It permits only
`Hello → Heartbeat → ShutdownAck`; it cannot invoke authority actions. The
legacy `KernelService` and `PluginLifecycleService` remain compatibility
projections only. Principal injection and UDS peer authorization are still
required for full conformance; filesystem access to a local UDS remains only a
provisional local admission boundary.

The canonical projection now also carries semantic `Worker`, `Operation` and
`EventPage` actions. `StartWorker` accepts only an opaque execution reference;
the out-of-Kernel resolver proves the digest-bound installation before returning
a launch plan. `ReadEvents` is the finite `EventCursor → EventPage` projection.
`WatchEvents` is the continuous server stream: it replays `Event` values, then
emits a typed `EventContinuity` frame for `GAP` or `SOURCE_CHANGED` and closes.
Neither operation reuses the legacy `resume_token` or LRO event
envelope. The runtime serves authority actions on one UDS path and canonical
plus compatibility Worker control actions on a separate Worker UDS path, so a
Worker control client is never registered on the authority endpoint.

The Node Agent projects finite `ReadEvents` through a typed
`KernelAuthorityCommand`/`KernelAuthorityCommandResult` control-stream branch.
Its one-command/one-result envelope does not create a durable subscription or
pretend to be a continuous `WatchEvents` stream; continuous observation uses
the canonical authority gRPC stream. It offers and validates the selected
semantic revision during its fenced Node session, then forwards only the
canonical action request to the local Authority UDS client. Semantic results
remain typed; a gRPC rejection trailer is carried back as a serialized
`cyrene.semantic.v1.Rejection` detail so remote Kotlin, Python and native
clients can branch on `reason_code` rather than an error string. Legacy node
commands remain a separately named compatibility branch.

Installation remains outside the authority. The built-in filesystem resolver
accepts a Worker `execution_ref` only in its adapter-owned
`installation-name@sha256:digest` form, verifies the immutable installation
record, and returns the digest-bound launch plan through the resolver port.
OCI retrieval, signatures, SBOM/provenance policy and language SDK packaging
therefore stay out of Kernel code; a different installer may implement the
same resolver port without changing the semantic API.

Changing an existing field meaning, accepted input, authority decision,
transition, reason code or bound requires semantic v2. Additive optional
projection fields are v1-compatible only when an older implementation can
ignore them without changing any v1 decision. Protobuf field numbers and enum
numbers are never reused. Freeze enforcement compares PRs with the target
branch/released descriptor and runs the shared TCK; a checked-in descriptor
cannot be treated as the only independent compatibility proof.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Kernel Semantic Contract v1

状态：**Frozen v1.0**（2026-08-11）。

本文档是 Kernel 语义的唯一规范 authority。Protobuf schema、Rust model、C ABI shim、JVM/Python SDK 和 transport 都是 projection。语言无关的 TCK 是本文档的可执行验收 projection。若 projection 与本文档冲突，则 projection 有缺陷，不能据此默认为契约已变更。

## 1. 目的与边界

Kernel 是节点本地的 authority，负责 identity、ownership、allocation、fencing、有界生命周期转换和有序 observation。它不是 AI framework、package manager、hardware driver、container engine、scheduler、network proxy 或 business service。符合规范的实现可以用任何内存安全语言重写，不必保留 Rust 类型或模块布局。

九个持久名词为：

1. Principal —— 由 transport boundary 派生出的 authenticated actor。
2. Provider —— 经认证的进程外事实或执行来源。
3. Resource —— 由 Provider 拥有、以通用方式描述且可分配的单元。
4. Lease —— 对 Resource 的排他、有限且带 fence 的 authority。
5. Worker —— 位于外部故障域中的执行 identity。
6. Operation —— 有界且可取消的生命周期转换。
7. Capability —— 带 namespace 和 revision 的精确匹配事实。
8. Endpoint —— 直接数据通路的授权元数据。
9. Event —— 不可变的有序 observation，不是 callback。

Identity、ContractRevision、ResourceQuery、Quantity、EndpointGrant、EventCursor、EventPage、EventContinuity 和 Rejection 是有界 value object。Product、model、dataset、runtime-language、container 和 hardware-vendor 词汇不属于 Kernel 语义。

## 2. Revision 与 projection 规则

语义 revision 为 { contract_id = "cyrene.kernel.semantic", major = 1, minor = 0 }。它在任何改变状态的 action 之前协商，并且与 gRPC、UDS framing、Protobuf package、Worker protocol 或 Adapter protocol 版本相互独立。只有 contract ID 和 major 相同时 revision 才兼容；选用双方支持的较小 minor。不兼容时必须 fail closed，并返回 CONTRACT_INCOMPATIBLE。

每个 projection 必须：

- 在 caller 控制的 message body 之外认证 Principal；
- 拒绝缺失的必填 message field、UNSPECIFIED state 和未知 enum number，不得自行填入默认值；
- 精确保留 ID、generation、fence token、quantity 和 timestamp；
- 即使把稳定的 reason_code 映射到本地 status 机制，也必须保留其原值；
- 仅当忽略未知 optional field 不会削弱 v1 authority 或 lifecycle decision 时才可忽略；
- 不得在 semantic wire contract 中放入 native handle、pointer、PID、file descriptor、device handle、credential 或 implementation path。

C ABI 是 client/projection boundary，不是进程内 plugin ABI。Vendor library 和不受信任的 extension 必须留在进程外，并通过有界的认证 transport（例如 UDS）连接。

## 3. 通用有效性与边界

不透明 identity ID 必须是非空 UTF-8，不含控制字符，长度至多 256 byte。generation 不得为零。Identity 相等比较必须同时比较两个字段。包含 Identity 的字段决定其 noun 类型：例如 Resource.provider 引用 Provider，Worker.lease 引用 Lease；不同 noun 的 ID 只需在各自 noun 范围内唯一。

Namespaced identifier 最长 128 个 ASCII byte。它由匹配 [a-z][a-z0-9]* 的 segment 组成，segment 间只能使用一个点、连字符或下划线。空 segment、大写字母以及开头或结尾的分隔符都无效。Provider observation reason identifier 遵循该语法。语义 Rejection.reason_code 则使用 upper snake case [A-Z][A-Z0-9_]*，同样最多 128 byte。

Timestamp 是精确的 Unix 毫秒，范围为 1..=253402300799999。Protobuf Timestamp projection 必须拒绝越界值以及无法精确表示为毫秒的 nanosecond。若 noun 允许，deadline 可以缺省；Lease、EndpointGrant 和 ProviderSnapshot 的 expiry 必填。

| 边界项 | 冻结的 v1 值 |
|---|---:|
| Identity ID | 256 byte |
| Namespaced ID / reason code | 128 byte |
| 每个 object 的 Capability 数量 | 64 |
| Property、capacity 条目、topology link 或 Worker limit 数量 | 64 |
| 每个 ProviderSnapshot 的 Resource 数量 | 1,024 |
| 每个 Lease / ResourceQuery count 的 Resource 数量 | 256 |
| 每个 ProviderSnapshot 的 Worker 或 Endpoint 数量 | 各 4,096 |
| Worker execution reference | 512 byte |
| Rejection message | 1,024 byte |
| Event body | 65,536 byte |
| 每页 Event 数量 | 256 |

Map 使用 namespaced key；value 必须非空、最多 256 个 UTF-8 byte 且不含控制字符。重复 capability 和 requirement 不得包含重复的 capability ID。完整 snapshot 中，稳定的 Resource、Worker 或 Endpoint ID 不得对应多个 incarnation。

## 4. Identity、generation 与 fencing

Generation 描述 incarnation，而不是普通可变状态：

- 每当 Provider 建立新的认证 session/process incarnation 时，其 generation 都改变；
- 只有当物理/逻辑 allocatable unit 被替换或无法证明连续性时，Resource generation 才改变；capacity、temperature、free memory 和 health 的变化不会改变它；
- ProviderSnapshot 的 snapshot_generation 是单独的序号，只在一个 Provider generation 内有效；每次接受完整 publication 时都必须严格递增；
- Worker、Lease、Operation 和 Endpoint 在终止/替换后，只有使用更高 generation 才可复用 ID。

Provider restart 不得重置旧 Provider generation。只有独立证明连续性时 Resource 才可保留原 generation；否则必须递增。Snapshot generation 绝不能复制到 Resource generation。

Fence token 必须非零，由 Kernel 单调分配；即使 restart 后，同一 node authority 下也不得复用。Lease 对外可见之前，下一段 token 范围必须已持久预留；无法保证该属性属于 startup/allocation failure。Generation 和 fence 都必须检查，不能相互替代。

## 5. Principal、Provider、Resource 与 Capability

Transport 通过 UDS peer credential、mTLS 或等效可信机制认证 Principal，并将其注入 KernelAuthority。请求字段不能自行声明 Principal。Authorization policy 可以决定某个 Principal 拥有哪些 Worker ID，但不能削弱 generation 或 fence 检查。

Provider 必须先注册一个经过认证的 incarnation，之后才能发布事实。它的 READY、DEGRADED、UNAVAILABLE state 及 capability 都是 observation。Provider 故障按 Provider 隔离，不得因此让无关 Provider 也变为 unavailable。

Capability matching 仅支持：

- capability ID 精确相等；
- provided.revision >= required.minimum_revision；
- 对显式要求的 string property 做精确相等比较；
- unit 完全一致时比较 unsigned scalar。

Kernel 中没有 expression language、script、callback、regex engine 或 Provider 提供的代码。复杂 scheduling 必须在 Kernel 外编译为有界的 ResourceQuery。

Resource class 是不透明值，例如 accelerator 或 execution.slot。Kernel 不解释 class、capability、capacity 或 attribute 名称。v1 Lease 仅支持排他。Provider 通过发布可分别 fencing 的 partition/slot Resource 表示安全共享；不得要求 Kernel 根据 vendor topology 推断共享。v1 query 原子地选择一个 class。异构 policy 必须在 Kernel 外转换为有序 acquisitions，或由 Provider 发布 composite Resource。

仅用于限制单个 Worker 的 CPU/memory 数值属于通用 Worker limits，不是额外 Lease grant。只有 Provider 显式发布对应 Resource 时，CPU/memory 才成为可分配 authority。

## 6. Lease authority 与生命周期

Lease 包含非空 Resource 集、一个计划中的 Worker holder、从 ACTIVE 出发的生命周期、非零 fence 和有限 expiry。仅当以下条件全部满足时，Lease 才授权某个 action：

- Lease 有效且为 ACTIVE；
- holder Identity 和 generation 匹配；
- Lease 中包含对应 Resource Identity 和 generation；
- 提交的 fence 与 Lease fence 相等；
- 当前时间严格早于 expiry。

即使尚未观察到 EXPIRED Event，expiry 到时也会立即移除 authority。Renewal 必须使用当前 identity/fence，不能缩短 expiry，并且要保留 identity、resources 和 fence。Replacement/revocation 必须推进 authority，使 stale Worker 无法重新连接。

合法 Lease 转换（自转换表示幂等重放）：

| 起始状态 | 其他合法目标 |
|---|---|
| ACTIVE | RELEASING、EXPIRED、REVOKED、FAILED |
| RELEASING | RELEASED、REVOKED、FAILED |
| RELEASED、EXPIRED、REVOKED、FAILED | 无；终态 |

因此 Release 流程为 ACTIVE → RELEASING → RELEASED；实现可以不对外显示中间 observation，但不能直接修改到终态。

## 7. Worker 与 Operation 生命周期

Worker 必须且只能引用一个 Principal、一个 Provider 和一个 Lease，并携带经过进程外验证、不可变的 execution reference。其通用 limit 由外部 execution/sandbox Provider 强制执行。Kernel 不在 Worker 中保存 command、environment、native process handle 或 vendor runtime object。

合法 Worker 转换：

| 起始状态 | 其他合法目标 |
|---|---|
| REGISTERED | STARTING、DRAINING、STOPPED、FAILED、LOST |
| STARTING | RUNNING、DRAINING、STOPPED、FAILED、LOST |
| RUNNING | DRAINING、STOPPED、FAILED、LOST |
| DRAINING | STOPPED、FAILED、LOST |
| STOPPED、FAILED、LOST | 无；终态 |

Operation 有一个 Principal owner、一个 Provider/Worker executor、一个不透明的 namespaced kind、可选 deadline、可选 parent Operation，以及有界 metadata。其生命周期为：

| 起始状态 | 其他合法目标 |
|---|---|
| CREATED | PENDING、RUNNING、CANCELLING、CANCELLED、FAILED |
| PENDING | RUNNING、CANCELLING、CANCELLED、FAILED、LOST |
| RUNNING | SUCCEEDED、FAILED、CANCELLING、LOST |
| CANCELLING | CANCELLED、FAILED、LOST |
| SUCCEEDED、FAILED、CANCELLED、LOST | 无；终态 |

自转换表示幂等重放。其他转换一律以 STATE_TRANSITION_INVALID 拒绝；restart 必须使用更高 object generation。

## 8. Endpoint 授权

Endpoint 只包含 metadata，其数据不会经过 Kernel。Provider 只有在 Principal 获准管理相应 Worker，且该 Worker 已存在时，才可发布 Endpoint。只有该 owner Principal 或 Kernel revocation policy 可以授权/撤销 grant。

EndpointGrant 只有在以下条件全部满足时才有效：

- grant、Endpoint、grantee、Lease 的 identity 与 generation 相符；
- grantee 是被引用 Lease 的 holder；
- grant fence 等于活动 Lease fence；
- Lease 和 grant 的 expiry 都尚未到达。

Credential 由 Endpoint Provider 通过带外方式交付，绝不能出现在 Endpoint public attribute 或 Event 中。



## 9. Event 顺序与重放

每个 Event 都携带 Kernel/node authority 的 source Identity。同一 source generation 内的 sequence 必须严格递增；resume cursor 是二元组 (source, sequence)。Event body 可以是基于已注册 schema ID 的描述性 payload，也可以缺省。它不得携带 authority、credential、native handle，或解释 Event header 所必需的 state。

ReadEvents 返回至多包含 256 个 Event 的 EventPage：

- CURRENT：source 相同，且保留的 history 覆盖 sequence + 1；Event 严格有序，next_sequence 等于最后一个返回 Event 的 sequence；
- GAP：source 相同，但请求的 history 已被淘汰；不返回增量 Event，必须重新获取 snapshot 并 reconciliation；
- SOURCE_CHANGED：Kernel authority incarnation 不同；不返回增量 Event，必须重新获取 snapshot 并 reconciliation。

空的 current page 保留输入 cursor。绝不能把 gap/source change 静默转换为最早的保留 Event。

如果所选 event store 提供 durable history，它就是同一 source replay 和 authority snapshot cursor 的规范来源；内存 event cache 不得缩短该 history。Durable read 出错或数据损坏时必须 fail closed，不能退回 cache。出现 GAP 或 SOURCE_CHANGED 后，client 根据现有 authority snapshot 重建状态，并从返回的 cursor 继续。更早 source generation 的 Event 仅是审计历史，不能用于重建新 Kernel epoch 的活动 authority state。

WatchEvents 从相同的 cursor continuity 判断开始，以 server stream 重放保留的 Event，然后继续观察 live authority。每个完整 Event 都作为 Event 发出。GAP 或 SOURCE_CHANGED 会以 typed EventContinuity stream frame 发出并关闭 stream；不得将其转换为通用 transport status。EventPage 仅用于有限的 ReadEvents result。ReadEvents 和 WatchEvents 共用相同的 cursor、source identity 和 durable history 规则。

## 10. Provider snapshot 与 reconciliation

ProviderSnapshot 是完整的、有 expiry 的 snapshot，且只属于一个已注册的 Provider generation。每个包含的 Resource、Worker 和 Endpoint 都必须通过校验并引用该 Provider。缺失/过期的事实对新 authority 不可用，绝不能覆盖 Kernel 拥有的 Lease、fence 或 Operation state。

Reconciliation 必须按 Provider fail closed：

- 拒绝过期的 Provider 或 snapshot generation；
- 对新 Lease 而言，过期/缺失的 Resource 变为 UNAVAILABLE；
- 已有 Lease 保持当前 lifecycle state，但不能再从 unavailable fact 获得新 authority；Kernel 会发出 degraded/unavailable Resource/Worker Event，明确的 policy 可以将 Lease 转为 RELEASING、REVOKED 或 FAILED；
- 缺失的 Worker 变为 LOST，随后撤销其 Lease；
- Kernel 绝不接纳未经证明的 process、Resource owner 或旧 fence。

特意不定义 DEGRADED Lease state：health 是 observation，Lease state 则是 authority lifecycle。

## 11. 稳定 action 与 rejection

v1 action 为 NEGOTIATE、REGISTER_PROVIDER、PUBLISH_INVENTORY、RECONCILE_PROVIDER、ACQUIRE_LEASE、RENEW_LEASE、RELEASE_LEASE、START_WORKER、HEARTBEAT_WORKER、STOP_WORKER、CREATE_OPERATION、REPORT_OPERATION、CANCEL_OPERATION、PUBLISH_ENDPOINT、AUTHORIZE_ENDPOINT、REVOKE_ENDPOINT、READ_EVENTS 和 WATCH_EVENTS。Transport 可以合并入口，但不能省略其 validation 或 authority semantics。

稳定的 action-level reason code 如下：

- compatibility/authentication：CONTRACT_INCOMPATIBLE、AUTHENTICATION_REQUIRED、AUTHORITY_DENIED；
- projection validity：REQUIRED_FIELD_MISSING、UNKNOWN_ENUM_VALUE、TEXT_INVALID、NAMESPACED_ID_INVALID、REASON_CODE_INVALID、TIMESTAMP_INVALID；
- authority：GENERATION_INVALID、STALE_GENERATION、FENCE_TOKEN_INVALID、FENCE_MISMATCH、LEASE_EXPIRED、LEASE_NOT_ACTIVE、LEASE_RENEWAL_INVALID；
- lifecycle/replay：STATE_TRANSITION_INVALID、REPLAY_GAP、EVENT_SOURCE_CHANGED。

Rust reference projection 和 TCK 中针对 object 的 validation code 在 v1 中也保持稳定。人类可读 message 是有界诊断信息，不是稳定的程序逻辑。Projection 可以将 reason code 映射到 gRPC/errno/exception status，但必须原样保留该 reason code。

## 12. 兼容性、一致性与演进

现有 cyrene.core.v1 中以 Plugin 命名的 API 是临时 compatibility projection。它们不是 semantic authority，不得被用来弱化 Principal、TTL、generation、fence、transition 或 replay 行为。新的 public API 使用 semantic noun 和 KernelAuthority；兼容性转换留在最外层 transport boundary。

在本次冻结时，specification、semantic Proto、纯 Rust model 和 Kernel-semantic TCK 共同定义 v1。当前 API/semantic projection 状态明确记录如下；deployment hardening 单独跟踪，不会降低已完成的 naming 或 contract projection 状态。

| 组件 | 当前状态 | 证据与准确范围 |
|---|---|---|
| Core gRPC | **COMPLETE** | v1/v2 提供有限的 ReadEvents 和连续的 WatchEvents；ReadEvents 返回 EventPage，而 WatchEvents 在 GAP 和 SOURCE_CHANGED 时发出 typed EventContinuity frame；daemon RPC test 覆盖 replay、handoff、backpressure 与 reconnect。 |
| Node Agent | **COMPLETE** | 一次 command/一次 result 的 Node control projection 携带有限 ReadEvents 和 EventPage result；连续 WatchEvents 对该 envelope 标为 NOT_APPLICABLE，并继续使用规范 Core gRPC stream。Node bridge 与 integration test 通过。 |
| Hardware Adapter | Event read/watch 为 **NOT_APPLICABLE** | 本地 adapter contract 报告 inventory fact 并创建 resource binding；它不拥有 durable Event history 或 observation。Hardware adapter protocol round-trip 与 daemon integration test 通过。 |
| Sandbox Adapter | Event read/watch 为 **NOT_APPLICABLE** | 本地 adapter contract 启动、停止、观察并恢复具体 process；它不拥有 durable Event history 或 cursor continuity。Sandbox protocol 与 UDS integration test 通过。 |
| Rust semantic authority | **COMPLETE** | Event、EventCursor、EventPage、EventContinuity 和共享的 ReplayStatus 定义一个语义模型；KernelAuthority::read_events 是两个 projection 共用的有限 history operation。Rust semantic TCK 和 workspace test 通过。 |
| Python consumer / TCK | **COMPLETE** | 仓库中已跟踪的 Python Kernel Semantic TCK 通过；Platform 不拥有第二套 Python Event authority 或替代 operation 名称。 |
| Kotlin TCK / consumer | **COMPLETE** | 仓库中已跟踪的 Kotlin Kernel Semantic TCK 使用相同冻结 vectors 和 semantic name 通过。 |

剩余的 PRODUCTION_HARDENING / DEPLOYMENT_VALIDATION 项包括跨 host mTLS、systemd crash recovery、特权 deployment identity 和多账号 Linux 验收。它们不是缺失的 API operation、projection 或命名规则。

KernelAuthorityService 是规范的 Core gRPC projection。每个改变状态的 authority action 都必须在 AuthorityCallContext 中带上选定的 ContractRevision；它不携带 caller 提供的 Principal、NodeRef、plugin installation 或 vendor-specific data。WorkerControlService 是独立且受限的双向 liveness/drain projection：其 hello frame 只包含已授权的 Worker 和 Lease identity 以及 fence，Kernel 返回权威 Worker record。它只允许 Hello → Heartbeat → ShutdownAck，不得调用 authority action。Legacy KernelService 和 PluginLifecycleService 仅保留为 compatibility projection。完整一致性仍要求 Principal injection 和 UDS peer authorization；访问本地 UDS 的 filesystem permission 仍只是临时的本地准入边界。

规范 projection 现在也承载 semantic Worker、Operation 和 EventPage action。StartWorker 只接受不透明的 execution reference；Kernel 外的 resolver 必须在返回 launch plan 前证明该安装与 digest 绑定。ReadEvents 是有限的 EventCursor → EventPage projection。WatchEvents 是连续 server stream：先重放 Event 值，之后在 GAP 或 SOURCE_CHANGED 时发出 typed EventContinuity frame 并关闭。两种 operation 均不复用 legacy resume_token 或 LRO event envelope。Runtime 在一条 UDS path 上提供 authority action，并在另一条独立的 Worker UDS path 上提供规范及兼容 Worker control action，因此 Worker control client 不会被注册到 authority endpoint。

Node Agent 通过 typed KernelAuthorityCommand/KernelAuthorityCommandResult control-stream branch 投影有限的 ReadEvents。其一次 command/一次 result envelope 不会创建 durable subscription，也不伪装成连续的 WatchEvents stream；连续 observation 使用规范 authority gRPC stream。它在受 fence 保护的 Node session 中提供并校验所选 semantic revision，然后只把规范 action request 转发给本地 Authority UDS client。Semantic result 保持 typed；gRPC rejection trailer 则作为序列化后的 cyrene.semantic.v1.Rejection detail 带回，使远端 Kotlin、Python 和 native client 可依据 reason_code 分支，而不必解析 error string。Legacy node command 保留为单独命名的 compatibility branch。

Installation 仍在 authority 之外。内置 filesystem resolver 只接受 adapter 所有的 installation-name@sha256:digest 格式 Worker execution_ref，验证不可变的 installation record，并通过 resolver port 返回 digest-bound launch plan。OCI retrieval、signature、SBOM/provenance policy 和语言 SDK packaging 因而都留在 Kernel code 之外；其他 installer 也可实现同一 resolver port，而不改变 semantic API。

改变已有 field 的含义、可接受输入、authority decision、transition、reason code 或边界都需要 semantic v2。只有当旧实现忽略新增 optional projection field 时不会改变任何 v1 decision，该字段才与 v1 兼容。Protobuf field number 和 enum number 永不复用。Freeze enforcement 将 PR 与目标分支/已发布 descriptor 比较并运行共享 TCK；不能把仓库内的 descriptor 当作唯一独立兼容性证明。
