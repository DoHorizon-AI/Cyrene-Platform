# CYRENE Platform Component Boundary v1

Status: **Normative**

Platform owns Kernel semantics, generic control-plane policy, verified process
supervision, Artifact Plane primitives, and Plugin lifecycle/discovery. Products
own application state and workflow. Plugin repositories own reusable capability
payload contracts, launchers, SDKs, implementations, and TCKs.

Dependency direction is:

```text
Product ──control request──► Platform ──authority action──► Kernel
   │                            │
   └──direct business call──► Plugin endpoint
                                ▲
                     Platform starts/observes process only
```

A Product or Plugin never imports Kernel private crates or calls a private
Kernel socket. Platform never imports Product/Plugin implementation code and
never handles business request or response payloads. Vendor hardware code runs
in an authenticated Adapter Host and is reduced to generic resource facts and
actions.

A new Platform primitive requires two independent consumers or a Kernel-level
invariant. Single-consumer adapters, Product manifests, deployment assets,
ecosystem catalogs, and migration snapshots stay in their owner repository.
