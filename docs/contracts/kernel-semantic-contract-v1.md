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
`EventCursor`, `EventPage` and `Rejection` are bounded value objects. Product,
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

`events_after` returns an `EventPage` of at most 256 events:

- `CURRENT`: same source and retained history covers `sequence + 1`; events
  are strictly ordered and `next_sequence` equals the last returned sequence;
- `GAP`: source matches but requested history was evicted; no incremental
  events are returned and a fresh snapshot/reconcile is required;
- `SOURCE_CHANGED`: Kernel authority incarnation differs; no incremental
  events are returned and a fresh snapshot/reconcile is required.

An empty current page preserves the input cursor. Gap/source change must never
be silently converted into the oldest retained event.

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
`AUTHORIZE_ENDPOINT`, `REVOKE_ENDPOINT` and `SUBSCRIBE_EVENTS`. A transport may
combine entry points but cannot omit their validation or authority semantics.

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
Kernel-semantic TCK define v1. The production Core gRPC service, Node Agent and
Hardware Adapter protocols are **partial migration projections**, not yet a
claim of full v1 conformance. In particular they must still adopt authenticated
Principal injection, semantic Worker/Operation/Event types, Provider-scoped
TTL/reconciliation, non-reused durable fences and the typed replay result.

`KernelAuthorityService` is the canonical Core gRPC projection. Its current
lease renewal and Endpoint actions require a selected `ContractRevision` in an
`AuthorityCallContext`; they carry no caller-supplied Principal, `NodeRef`,
plugin installation or vendor-specific data. The legacy `KernelService` and
`PluginLifecycleService` remain compatibility projections only. Until the
authority and Worker-control sockets are separated and Principal injection is
fully enforced, filesystem access to the local UDS is the provisional local
admission boundary rather than a claim of complete authorization conformance.

The canonical projection now also carries semantic `Worker`, `Operation` and
`EventPage` actions. `StartWorker` accepts only an opaque execution reference;
the out-of-Kernel resolver proves the digest-bound installation before returning
a launch plan. `SubscribeEvents` is a bounded pull projection of
`EventCursor → EventPage`: it returns typed `CURRENT`, `GAP` or
`SOURCE_CHANGED` and never reuses the legacy `resume_token` or LRO event
envelope. The runtime serves authority actions and Worker-control compatibility
actions on distinct UDS paths so a Worker control client is not registered on
the authority endpoint.

Changing an existing field meaning, accepted input, authority decision,
transition, reason code or bound requires semantic v2. Additive optional
projection fields are v1-compatible only when an older implementation can
ignore them without changing any v1 decision. Protobuf field numbers and enum
numbers are never reused. Freeze enforcement compares PRs with the target
branch/released descriptor and runs the shared TCK; a checked-in descriptor
cannot be treated as the only independent compatibility proof.
