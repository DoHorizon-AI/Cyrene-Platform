# CYRENE Kernel Semantic Contract v1

Status: candidate for freeze. This document is normative for Kernel semantics;
Protobuf, C ABI, JVM and Python APIs are projections of it.

## 1. Purpose

The Kernel is a node-local authority for identity, ownership and observed
runtime state. It is not an AI framework, package manager, device driver,
container engine or data-plane proxy. A conforming implementation may be
rewritten in any safe language if it preserves this contract.

The contract has nine durable nouns:

1. `Principal` — the authenticated actor. A transport adapter derives it from
   peer identity; callers cannot self-assert it.
2. `Provider` — an authenticated out-of-process source of facts or execution.
3. `Resource` — a provider-owned, allocatable object described by class,
   capabilities and scalar capacity.
4. `Lease` — the exclusive or explicitly shared authority to use resources.
5. `Worker` — an execution identity in an external failure domain.
6. `Operation` — a bounded, cancellable lifecycle transition.
7. `Capability` — a namespaced fact with an integer revision and bounded
   exact-match properties.
8. `Endpoint` — authorization metadata for a direct data path.
9. `Event` — an immutable, ordered observation of a state transition.

Names such as model, dataset, runtime language, container implementation,
accelerator vendor and framework are outside this vocabulary.

## 2. Identity and authority

Every mutable object has an opaque `Identity { id, generation }`. IDs are
stable names; generation identifies one incarnation. Neither the semantic
contract nor its public projections expose process IDs, file descriptors,
device handles, cgroup paths or pointers.

Every state-changing command is evaluated in an authenticated `Principal`
context. Resource-affecting actions must carry the current lease identity and
fence token. A lease authorizes an action only when all are true:

- the lease is active;
- the holder identity and generation match;
- the resource identity and generation are included;
- the supplied fence token equals the lease fence token;
- the lease has not expired.

Revocation or replacement increments generation and/or advances the fence.
An old Worker therefore cannot regain authority by reconnecting.

## 3. Capabilities and resources

Capability matching is intentionally not a policy language. Kernel v1 only
supports:

- exact namespaced capability ID equality;
- `provided.revision >= required.minimum_revision`;
- exact equality for explicitly required string properties;
- unsigned scalar capacity comparisons with identical units.

There is no expression evaluator, embedded schema engine, regular expression,
script, callback or provider-supplied code in the Kernel. Rich scheduling
policy belongs to the control plane, which compiles policy into this bounded
query form.

A `Resource` has an opaque class such as `accelerator`, but the Kernel does not
assign meaning to class or capability IDs. A provider may publish common
capabilities such as `accelerator.compute` and vendor-specific capabilities in
its own namespace. Adding new hardware must not require a new Kernel enum.

Provider inventory is a complete, generation-numbered, expiring snapshot.
Provider facts are observations; they never override Kernel-owned leases,
fences, Workers or Operations.

## 4. Workers and operations

Workers always execute outside the Kernel process. The Kernel stores only
their identity, provider, principal, lease, lifecycle state and an immutable
execution reference. Executable paths, arguments and environment belong to a
verified external resolver and execution provider.

The stable Worker state set is `REGISTERED`, `STARTING`, `RUNNING`, `DRAINING`,
`STOPPED`, `FAILED` and `LOST`. Provider-specific phases are metadata or
events, not new Kernel states.

The stable Operation state set is `CREATED`, `PENDING`, `RUNNING`,
`SUCCEEDED`, `FAILED`, `CANCELLING`, `CANCELLED` and `LOST`. Operation `kind`
is a namespaced opaque identifier. The Kernel coordinates lifecycle and
deadline semantics without interpreting business meaning.

## 5. Endpoints and events

An Endpoint describes who owns a data path, which provider created it, its
transport identifier, schema identifier and public attributes. The Kernel may
publish, authorize and revoke Endpoint grants. It never proxies endpoint data
and never places transport credentials or native handles in Events.

Events are facts, not callbacks. Each Event has a node-local monotonic
sequence, subject identity, namespaced kind, timestamp, schema identifier and
a bounded opaque body. Streams may retain a bounded replay window; consumers
that fall behind must obtain a fresh snapshot and reconcile.

## 6. Provider reconciliation

Providers authenticate, negotiate a protocol version, publish complete
snapshots and reconcile after either side restarts. Reconciliation compares
provider observations with Kernel authority:

- missing or expired resources become unavailable for new leases;
- existing leases are marked degraded and handled by explicit policy;
- missing Workers become `LOST`, then their leases are revoked;
- provider claims with stale generation are rejected;
- the Kernel never adopts an unproved process or resource owner.

## 7. Stable actions

The v1 semantic action set is:

- `NEGOTIATE`
- `REGISTER_PROVIDER`, `PUBLISH_INVENTORY`, `RECONCILE_PROVIDER`
- `ACQUIRE_LEASE`, `RENEW_LEASE`, `RELEASE_LEASE`
- `START_WORKER`, `HEARTBEAT_WORKER`, `STOP_WORKER`
- `CREATE_OPERATION`, `REPORT_OPERATION`, `CANCEL_OPERATION`
- `PUBLISH_ENDPOINT`, `AUTHORIZE_ENDPOINT`, `REVOKE_ENDPOINT`
- `SUBSCRIBE_EVENTS`

New implementations may combine these actions into fewer RPC entry points,
but they may not weaken identity, generation, fencing, boundedness or event
ordering.

## 8. Compatibility and projections

The semantic version is negotiated independently from transport and schema
versions. A projection must preserve the same validation and state-machine
rules. Opaque native handles belong to a client library, never to the Kernel
wire contract or process address space.

Existing `cyrene.core.v1` Plugin-named RPCs are a compatibility projection
during migration. New API work must use the semantic nouns above. Compatibility
adapters may translate old requests but may not introduce old vocabulary into
the semantic package.

## 9. Boundedness

The reference projection fixes conservative limits: 256-byte identity IDs,
128-byte namespaced identifiers, 64 capabilities per object, 64 properties per
capability, 1,024 resources per provider snapshot and 64 KiB per Event body.
Transport projections may choose smaller limits but never larger ones without
a semantic contract revision.
