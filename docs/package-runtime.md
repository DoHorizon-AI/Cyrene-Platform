# Platform package runtime

`cy-package-runtime` is the Platform/Control Plane production seam for generic
capability package lifecycle. It consumes the existing Workspace Package Spec
descriptor and the existing Official Plugins `plugin.manifest.json`; it does
not define another manifest, catalog, registry, or Product policy model.

The runtime supports inspection, verification, installation, durable get/list,
locked dependency preparation, binding activation/deactivation, process-backed
runtime status, upgrade, rollback, uninstall protection, offline reinstall,
and cache/staging reclamation. Worker execution delegates to
`CapabilityWorkerActivator` and the existing `cy.plugin.v1` worker protocol.
CES remains the Product-facing capability execution boundary.

## Identity and state ownership

- `PackageId`, `PackageVersion`, and `ArtifactDigest` identify immutable package
  input.
- `InstallationId` identifies one digest-bound local publication and is never a
  package ID.
- `CapabilityId` is the callable contract identity.
- `BindingId` remains stable Product configuration identity.
- `RuntimeGeneration` changes on every activation, recovery, upgrade, or
  rollback and is never a binding or installation ID.
- `AVAILABLE` comes from the catalog, `VERIFIED` from verification evidence,
  `INSTALLED` from the atomic installation record, `CONFIGURED` from Product
  binding state, `ENABLED` from Product policy, and `RUNNING` only from the
  worker supervisor's child-process poll.

## Install transaction

Installation validates the published descriptor, archive digest, member paths,
symlink policy, extracted artifact digest, official manifest identity, and
dependency lock digest. It then prepares dependencies from the exact lock,
writes verification and preparation evidence inside an invisible staging
directory, fsyncs it, and atomically renames it beneath `installations/`.

Failed preparation never creates a final installation directory or an
`INSTALLED` record. Archive and dependency bytes are content-addressed for
repeat and offline installation. A deterministic, digest-derived
`InstallationId` makes repeat and concurrent installation idempotent.

Archive members containing traversal, absolute/platform-specific paths,
duplicates, or symbolic links are rejected before extraction. Activation
revalidates the installed payload and dependency evidence before worker spawn.

## Recovery and cleanup

Startup removes incomplete staging transactions but never synthesizes an
installed record. Durable activation records contain package-runtime identity
only; environment and secret values are not persisted. Product supplies those
values again to `recover_binding` after restart.

Uninstall rejects current and rollback references. Cache bytes remain available
for offline reinstall until explicit cleanup. Cleanup terminates any supervised
worker not backed by an active runtime record and reports the final orphan
count.

## Node-local control process

`cy-package-runtime --root <state-root>` owns the lifecycle and supervised
workers for one node. Adapters exchange one JSON request and response per line
using `cy-package-runtime.control.v1`. The channel exposes structured error
codes and remediation, and supports the same inspect, verify, install,
activation, recovery, upgrade, rollback, uninstall, and cleanup operations as
the Rust API.

Descriptor/archive paths, dependency paths, worker environment values, and the
worker protocol are internal to this node-local seam. A Product adapter must
project only package identity/version, installation identity, verification,
binding policy, runtime status, and structured failure/remediation. It must not
project cache, staging or virtualenv paths, ZIP internals, executables, stdio,
PIDs, or runtime implementation details.
