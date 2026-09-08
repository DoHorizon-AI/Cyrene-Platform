# CYRENE Platform Component Boundary v1

Status: **Normative transition boundary** (2026-08-12).

This contract applies the frozen Kernel Semantic Contract v1 and the CYRENE
Engine Architecture Blueprint to product services and reusable capabilities.
It does not add Kernel nouns, actions or authority.

## 1. Authority and dependency direction

```text
Kernel Semantic Contract -> Kernel wire -> C ABI / JVM client bindings
                                      ^
Platform Control Plane ----------------|
       ^                         ^
       | public API              | controlled process protocol
Service ---------------------- Capability Plugin
```

Dependencies only point upward through published contracts. A Service or
Capability Plugin never imports a Kernel private crate, calls a Kernel UDS,
uses a Kernel C handle, selects a GPU, or self-asserts a Principal. Only the
Platform, Node Agent and authenticated Adapter Hosts can invoke Kernel actions.

## 2. Component classes

| Class | Purpose | Runtime boundary | May use Kernel semantic API |
|---|---|---|---|
| `platform` | Kernel, Control Plane, generic policy and Plugin Host | trusted platform processes | Yes, by role |
| `adapter-host` | sandbox or hardware implementation detail | separate authenticated local process | Yes, restricted ingress |
| `capability-plugin` | reusable business capability | out-of-process worker or network service | No |
| `service` | user-facing or product-specific executable | independently deployed application | No |
| `service-bundle` | signed composition of services and capability plugins | metadata only | No |
| `infrastructure-asset` | deployment/edge template or operational asset | not a runtime plugin | No |
| `compatibility-snapshot` | extracted source retained for migration | not installable | No |

`adapter-host` is not an ecosystem business plugin. Vendor C ABIs stay inside
that host and are reduced to versioned, authenticated facts/actions on its
private UDS. A `capability-plugin` may request resources through its Service or
the Control Plane, but the Platform alone obtains and fences a Lease.

## 3. Artifact and protocol rules

- An installable `capability-plugin` uses one validated `plugin.toml`, a
  versioned extension contract, and a process protocol such as `cy.plugin.v1`.
  Its manifest/Hello identity, API version, protocol version, capability set,
  artifact digest and permissions must agree before activation.
- A product `service` uses `service.json`; it may declare required optional
  capabilities, but it never embeds or imports a plugin implementation.
- A `service-bundle` can carry `service.json` and several installable
  `plugin.toml` components in one signed OCI artifact. These are metadata of
  one installation path, not two independent installers.
- `compatibility-snapshot` and `infrastructure-asset` are owned and catalogued
  outside Platform. They are not discoverable or activatable by the Plugin Host
  and must not declare a fictitious executable runtime or capability.
- C ABI and JVM SPI versions remain independent from a plugin contract version
  and `cy.plugin.v1` protocol version. They are client bindings, not plugin APIs.

## 4. Binding corrections required before implementation

`docs/C ABI&JDK SPI.md` has the right direction but is a binding design guide,
not an independent semantic authority. Before any C ABI or JVM SPI is published:

1. A semantic Identity must retain both opaque `id` and non-zero
   `generation`; a local C handle is never a semantic ID.
2. A state-changing request needs the selected `ContractRevision`, request ID
   and idempotency key. Principal is transport-injected, never caller supplied.
3. The Contract Matrix must cover all 17 Kernel v1 actions, name caller roles
   and exact schemas, and preserve stable `Rejection.reason_code`.
4. Event bindings must preserve `EventCursor -> EventPage` with
   `CURRENT`, `GAP` and `SOURCE_CHANGED`; streams are convenience APIs only.
5. `ServiceLoader` is allowed only inside an external Adapter Host. Kernel,
   Control Plane, Services and business plugins must not load third-party
   Providers in process.

## 5. Consumer-owned migration surfaces

Products own their application behavior, deployment templates, protocol
adapters and compatibility snapshots. Reusable capabilities belong in the
Plugins repository only after they have an external contract, a runnable
worker or service, an activation path and a TCK. Platform does not retain a
copy while that extraction is in progress.

A Product-specific worker may use the generic process and capability contracts,
but its protocol and lifecycle remain consumer-owned until standardized. A
single consumer is not sufficient reason to add a Platform API.

## 6. Admission gates

A component can move from `compatibility-snapshot` to `capability-plugin`
only when all conditions hold:

1. The owning extension contract names its request, response, stream, cancel,
   error, authorization, configuration and event semantics.
2. The worker is out of process and has no direct database, Kernel, GPU or host
   object access outside declared, mediated permissions.
3. Its manifest is canonical, digest-bound and matches its runtime handshake.
4. It has a contract TCK including timeout, cancellation, restart, idempotency,
   permissions and failure isolation.
5. A Service invokes it through the Plugin Host rather than importing its code.

Present-state classifications belong to the repository that owns or composes
the component. Platform publishes the generic catalog schema but does not keep
an ecosystem-wide component instance registry.
