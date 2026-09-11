# CYRENE Kernel Design Goals

**Status:** long-lived design guidance; not a versioned wire contract or an
implementation checklist.

This document records the direction and invariants that guide Kernel evolution.
The exact v1 meanings, fields, error handling, and compatibility rules are
defined only by the [Kernel Semantic Contract v1](../contracts/kernel-semantic-contract-v1.md).
Work that is currently expected to be implemented or validated is tracked in
the [Kernel Execution Goals](../operations/kernel-execution-goals.md).

## Purpose and boundary

CYRENE Kernel is the local authority for a node's resources and their safe use.
It owns identity, resource observation, lease authority, fencing, local worker
lifecycle, local control-plane access, and the facts emitted about those
changes. It does not become a global scheduler, a vendor SDK, a device driver,
or a product/business layer.

The architectural repository boundary is the
[Platform clean boundary](../governance/platform-clean-boundary.md). The older
[Engine Architecture Blueprint](../CYRENE_ENGINE_ARCHITECTURE_BLUEPRINT.md) is
retained only as historical context.
Platform and resource-specific isolation stay in external adapters or services;
the Kernel keeps the common semantic authority.

## Design principles

1. **Semantic model before projection.** Kernel concepts are defined once in
   the semantic model. gRPC, UDS, C ABI, JVM, Kotlin, or other transport and
   language views are projections, not independent sources of truth.
2. **Local authority is explicit.** A node can authorize only resources and
   workers it owns. A resource must have one authoritative provider at a time,
   and every mutating action must be attributable to a principal.
3. **Leases carry authority and fencing.** A lease is finite, renewable, and
   associated with a fence. Stale authorities must be rejected rather than
   silently overriding a newer holder.
4. **Providers normalize heterogeneity.** Hardware, execution, network, and
   similar backends expose stable semantic capabilities and resources. Vendor
   details may be retained as provider metadata, but they must not leak into
   the Kernel's common authority model.
5. **Reconciliation repairs observed state.** Inventory reports what a
   provider sees; reconciliation decides whether it matches desired state and
   produces the necessary repair work. The two concerns must not be conflated.
6. **Workers are controlled local processes.** A worker has an owner, a
   generation, observable lifecycle, and a failure path. Kernel recovery may
   clean up or reconcile known work, but must not adopt an unverified foreign
   process.
7. **Control and data paths are distinct.** The Kernel authorizes and
   describes endpoints; it is not the high-throughput payload forwarding path.
   Endpoint exposure must retain ownership and authorization information.
8. **Events are facts with replay semantics.** Events describe completed state
   transitions and must preserve enough identity, ordering, and cursor
   information for clients to resume safely within the retained history.
9. **Isolation is a platform responsibility behind a boundary.** Process,
   cgroup, pidfd, device-filter, and GPU primitives belong to the sandbox or
   hardware adapter boundary. The Kernel depends on their declared outcomes,
   not on a specific OS or vendor implementation.

## Core vocabulary

The durable Kernel model uses the following nouns. The v1 contract defines
their concrete representations and valid transitions.

- **Principal:** authenticated actor whose authority is auditable.
- **Provider:** authoritative owner of a class of local resources.
- **Resource:** schedulable or allocatable local capability under one provider.
- **Capability:** stable, provider-normalized description of what a resource
  can do.
- **Lease:** time-bounded authority over one or more resources.
- **Fence:** monotonic authority token that rejects stale mutations.
- **Worker:** local execution instance bound to an owner and lifecycle.
- **Operation:** observable asynchronous unit of requested work.
- **Endpoint:** authorized connection description, not the payload relay itself.
- **Event:** durable fact about a semantic transition, available for bounded
  replay.

Identity is opaque and stable within its contract scope. A generation or fence
is never a cosmetic field: it is part of the stale-authority and recovery
story.

## Evolution discipline

The following are valuable design directions, but are **not** implicit v1
requirements: namespace-scoped identity, a snapshot-plus-stream event model,
server-streaming delivery, new provider actions, and additional ABI or JVM
projections. Each requires a proposal that states its versioning and
compatibility impact, updates the authoritative semantic contract, and adds
cross-projection conformance coverage before it becomes a promise.

Before accepting a Kernel semantic change, answer these questions:

1. Which core concept, authority rule, or state transition changes?
2. Is the change backward compatible for persisted state, clients, and
   adapters?
3. How do identity, generation, lease, and fence behave after restart or
   reconciliation?
4. Which contract and golden/TCK cases prove the behaviour in every supported
   projection?
5. Does the change preserve the Kernel's boundary rather than importing a
   product, scheduler, driver, or vendor concern?

## Related operational material

The [Kernel runtime baseline](../operations/kernel-runtime.md) describes the
current deployment and operating shape. It is intentionally separate from these
design goals: runtime instructions may evolve with platform support, whereas
the principles above change only through an explicit architectural decision.
