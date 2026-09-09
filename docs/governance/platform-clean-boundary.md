# Platform clean boundary

Status: **Normative**
Baseline: the commit that introduces this document and its CI guard
Validated deployment consumer: `Astrbot-Rev develop@685978cdff6fb06150e05e7aa9f66ebb87c85f0f`

Platform owns generic semantic and control contracts, Kernel and Node Agent
behavior, managed process and adapter-host boundaries, Lease/Fence rules, and
systemd units for Platform-owned daemons. Capability payload contracts, typed
SDKs and TCKs belong to Plugins or the Product owner. Platform does not own
Product deployment templates, edge configuration, observability composition,
protocol adapters, Product manifests, compatibility source snapshots, or an
ecosystem component catalog.

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
cross-repository component instances, Product-specific integration tests, and
business-payload operations in the package runtime if they are added back to
Platform.

The unused v0 named SPI, `cy-extension-registry`, `cy-local-transport`, ten
capability-specific Protobuf projections and `BuiltinInMemoryStorage` were
removed after repository-wide reverse-dependency checks found no production
consumer. Their old `Invoke` and `InvokeResult` field names and tag numbers are
reserved. The package runtime supervises a Plugin-owned service, validates its
readiness identity, and returns an opaque `connection_ref`; its control protocol
contains no invoke, subscribe, request, response, or event payload operation.

The remaining migration quarantine covers the v0 `plugin.toml` taxonomy and
AI-specific model/training/runtime records and JSON schemas in `cy-manifest`.
They remain frozen only until their current consumers move to their owner.

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

The media request adapter moved to Cyrene-Plugins-Official. Product code calls
the Plugin-owned endpoint directly. Platform does not know image/audio
operation names, capability-specific request classes, or their serialized
payloads.

## Evidence boundary

The Astrbot deployment test proves ownership of the migrated deployment assets.
It does not prove a live Kubernetes deployment. Hosted builds prove compilation,
unit tests, and contract TCKs; real GPU, Kubernetes, and external-provider
execution require their separate environment-specific acceptance evidence.
