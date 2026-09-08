# Platform clean boundary

Status: **Normative**
Baseline: the commit that introduces this document and its CI guard
Validated deployment consumer: `Astrbot-Rev develop@685978cdff6fb06150e05e7aa9f66ebb87c85f0f`

Platform owns the generic semantic contracts, Kernel and Node Agent behavior,
managed process and adapter-host boundaries, CES, Lease/Fence rules, SDKs, and
systemd units for Platform-owned daemons. It does not own Product deployment
templates, edge configuration, observability composition, protocol adapters,
Product manifests, compatibility source snapshots, or an ecosystem component
catalog.

The validated Astrbot consumer owns its Docker image, Kubernetes manifests,
NapCat composition, NGINX configuration and renderer. Azure build 443 passed
at the exact accepted revision above, including the .NET and PostgreSQL suite,
after the corresponding Platform copies were removed.

The other accepted migration destinations are:

- `Cyrene-Plugins-Official develop@4f730e1f13fbc64b04b533dbf7f4e90d06460604`
  owns the installable media worker adapter. Azure build 448 passed for its
  task head before normal merge and canonical ancestry read-back.
- `Cyrene-Yield develop@0f402e0fff5d07a001d90f97c35e211ffd1875b0`
  owns Product run, attempt, retry, persistence, model analysis, and
  compatibility-evaluation policy. Azure build 461 passed for task head
  `292971f6907294d59184d3a390c97974804b33aa` before normal merge; that head is
  an ancestor of the accepted revision.

## Extension rule

A Product or plugin must consume the published Platform contracts without a
Platform source change. If a generic primitive is missing, a `PLATFORM_GAP`
proposal must identify either two independent consumers or a Kernel-level
invariant and provide a contract test. Product convenience, deployment shape,
or protocol-specific behavior is not a Platform gap.

`tooling/ci/check-no-legacy-surface.sh` rejects consumer-owned infrastructure,
cross-repository component instances and Product-specific integration tests if
they are added back to Platform.

The implemented v0 typed SPI and `cy-extension-registry` are explicitly
`MIGRATING_COMPATIBILITY`. They have no production dependents in this
repository, accept no new capability-specific proxy, and remain only until the
Kotlin catalog/router conformance removal gate passes. Current integrations use
generic `CapabilityDescriptor`, CES typed payload forwarding, and WorkerControl.

The same quarantine includes `cy-local-transport`, the ten named SPI Protobuf
projections beside the generic worker envelope, `BuiltinInMemoryStorage`, the
v0 `plugin.toml` taxonomy, and the AI-specific v0 model/training/runtime records
and JSON schemas in `cy-manifest`. These paths contain implemented
compatibility behavior and therefore remain build-tested, but they are frozen
and are not current Product or Platform authorities.

The Python and Kotlin `ProductRun`/Attempt/retry/persistence implementations
were moved to Cyrene-Yield, which is their only source consumer. Their shared
Platform schema and fixture were moved with the Product lifecycle. Platform
retains no Product run store or reconciler. The default model analyzer,
compatibility evaluator, and their Product request and result types moved
behind Yield-owned replaceable ports. Platform `cyrene_preflight` now exposes
only resource facts and generic preflight results.

The empty `framework/jvm` Gradle shell was removed after its Product source
moved. Platform's JVM gate now builds only a generated contract consumer under
`tck/`; a new Product JVM application must live in its Product repository.

The media request adapter moved to Cyrene-Plugins-Official. Platform worker
dispatch forwards generic payloads and does not know image/audio operation
names or capability-specific request classes.

## Evidence boundary

The Astrbot deployment test proves ownership of the migrated deployment assets.
It does not prove a live Kubernetes deployment. Hosted builds prove compilation,
unit tests, and contract TCKs; real GPU, Kubernetes, and external-provider
execution require their separate environment-specific acceptance evidence.
