# Platform clean boundary

Status: **Normative**
Baseline: the commit that introduces this document and its CI guard
Validated consumer: `Astrbot-Rev develop@d14d60858a9c6b43b0b68a69a01ac76b69739b8b`

Platform owns the generic semantic contracts, Kernel and Node Agent behavior,
managed process and adapter-host boundaries, CES, Lease/Fence rules, SDKs, and
systemd units for Platform-owned daemons. It does not own Product deployment
templates, edge configuration, observability composition, protocol adapters,
Product manifests, compatibility source snapshots, or an ecosystem component
catalog.

The validated Astrbot consumer owns its Docker image, Kubernetes manifests,
NapCat composition, NGINX configuration and renderer. Its deployment control
plane suite passed 57 tests at the exact revision above before the corresponding
Platform copies were removed.

## Extension rule

A Product or plugin must consume the published Platform contracts without a
Platform source change. If a generic primitive is missing, a `PLATFORM_GAP`
proposal must identify either two independent consumers or a Kernel-level
invariant and provide a contract test. Product convenience, deployment shape,
or protocol-specific behavior is not a Platform gap.

`tooling/ci/check-no-legacy-surface.sh` rejects consumer-owned infrastructure,
cross-repository component instances and Product-specific integration tests if
they are added back to Platform.

## Evidence boundary

The Astrbot deployment test proves ownership of the migrated deployment assets.
It does not prove a live Kubernetes deployment. The removed JVM skeleton had no
runnable worker fixture and therefore remains `NOT_RUN`; it must return only as
an external environment gate with a real installation record and sandboxd
fixture.
