# Canonical Platform Plugin Contract v1

The Platform is the sole owner of the generic plugin and capability contract.
The Rust types in `contracts/rust/cy-manifest` are the canonical manifest model;
`framework/crates/cy-platform-api` owns the local registry and resolver policy.
Existing manifest fields remain compatible with the pre-v1 plugin schema.

## Contract inventory

- `PluginIdentity` and `PluginVersion` identify a plugin release.
- `CapabilityId` and `CapabilityInterfaceVersion` identify a callable contract
  separately from the plugin release.
- `CapabilityDescriptor` declares an interface and one or more `ExecutionMode`
  values: `INLINE`, `WORKER`, or `SERVICE`.
- `PluginArtifactRef` carries an already-known URI and optional digest. The MVP
  never downloads or materializes this reference.
- `PluginManifest` is the existing Platform manifest with additive
  `capability_descriptors` and `artifact` fields. Its legacy coarse capability
  table is retained for compatibility and is not used for interface matching.
- `PluginRequirement` (also exposed as `CapabilityRequirement`) requests an
  exact capability interface and optional execution-mode constraints.
- `PluginSetSpec` is a logical set of requirements. It does not imply one
  Python environment or one process.
- `ResolvedCapability` records the exact plugin/version, interface, mode,
  artifact, and compatibility evidence selected by the resolver.
- `PluginSetLock` sorts those records deterministically and carries a
  content-derived SHA-256 lock digest.
- `media.processor.v1` has a normative wire schema at
  `contracts/schemas/media-processor-v1.schema.json`; its implementation and
  typed language adapters belong outside Platform.

## Registry and resolver MVP

`CapabilityRegistry` is an in-memory/reference registry. Registration rejects
invalid descriptors and duplicate `plugin id + version` keys. Provider queries
are returned in deterministic order.

`CapabilityResolver` performs exact interface matching and intersects the
requirement's execution modes with the optional PluginSet global constraint.
Candidates are ordered by plugin identity, plugin version, and mode. The mode
ordering is `INLINE`, `WORKER`, `SERVICE`. Missing providers, interface
mismatches, and execution-mode mismatches are explicit errors; there is no
silent fallback.

## `media.processor.v1` first slice

The Platform-owned media wire schema contains bounded `inspect_image`,
`transform_image`, and `normalize_audio` payloads. Inputs distinguish caller-provided bytes from a caller-owned
file path; the contract has no URL, data-URI, stream, or opaque-handle string.
The transform surface is limited to resize, format conversion, codec quality,
and orientation normalization. Products retain attachment records, storage,
deduplication, authorization, persona/emotion semantics, and workflow policy.

The resolver accepts repository-facing `plugin.manifest.json` files
through a normalization adapter in `cy-platform-api`. That adapter feeds the
same `CapabilityRegistry` and `CapabilityResolver`; it is not a second manifest
authority or a direct-instantiation test shortcut.

The registry and resolver are local APIs only. Plugin Manager, Marketplace,
remote catalogs, installation, update, uninstall, and artifact download are
outside this contract.

## Service seams

Yield declares `training.engine.v1` through a small resolver protocol and wraps
the current `TrainingRuntime` as the reference provider. Reactor declares
`serving.engine.v1` and wraps the current lazy `EngineFactory` while retaining
the existing `InferenceServer` lifecycle. These adapters are replaceable seams,
not full engine extraction.

Exchange is truthfully deferred: the current live repository contains a
coordinator, not a gateway Product runtime. The preserved gateway sources can
adopt `gateway.runtime.v1` when a live owner and lifecycle seam exist.

## Contributor-ready follow-up

1. Extract the production TrainingEngine port from the Yield compatibility
   adapter and add a real Platform provider manifest.
2. Extract the production ServingEngine port from Reactor without moving
   Deployment readiness, traffic, or scaling policy out of the Service.
3. Add a second ModelProvider implementation and its interface compatibility
   tests.
4. Implement one or more GatewayRuntime variants after Exchange has a live
   gateway Product owner.
5. Extend PluginSet compatibility policy beyond exact interface versions and
   add policy-specific lock evidence.

None of these tasks require a Kernel semantic change.
