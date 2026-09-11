# Cyrene-Platform Contract Index

This index lists contracts physically owned by Cyrene-Platform. It is not a
catalog of Product or Plugin payloads and does not describe implementation
readiness.

| Surface | Platform source | Boundary |
| --- | --- | --- |
| Kernel semantic authority | `contracts/proto/cyrene/semantic/v1/kernel_contract.proto`, `docs/contracts/kernel-semantic-contract-v1.md` | Generic identity, lifecycle, Lease/Fence, endpoint, and event semantics |
| Kernel and Node control | `contracts/proto/cyrene/core/v1/`, `contracts/proto/cyrene/core/v2/` | Generic runtime, resource, process supervision, and node control |
| Execution and Artifact Fabric | `contracts/proto/cyrene/core/v1/node_control.proto`, `contracts/schemas/artifact_transfer.schema.json` | Placement, attachment, transfer, and failure facts |
| Artifact identity | `contracts/schemas/manifests/artifact_ref.schema.json`, `contracts/schemas/manifests/portable_directory_manifest.schema.json` | Immutable content identity with an opaque producer-owned category |
| Plugin resolution | `contracts/schemas/plugin-set.schema.json`, `contracts/rust/cy-manifest` | Generic capability identity, compatibility selection, and lock evidence |
| Package lifecycle | `contracts/proto/cyrene/core/v1/plugin_lifecycle.proto`, `contracts/schemas/verified-installation-record.schema.json` | Install, activate, supervise, health, and opaque `connection_ref` |

A capability contract, Product record, Plugin manifest, component catalog, or
business example belongs to its implementation repository. Adding one does not
require a Platform source change.
