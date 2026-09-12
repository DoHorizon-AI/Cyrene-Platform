# Public Package Release Order

This document is the release-engineering view of the public Rust package DAG.
It does not publish packages. The graph must be rechecked with Cargo metadata
before every versioned release.

## Current dependency order

Publish the leaf contracts first, wait for the registry index to expose each
version, then publish dependent packages:

```text
1. cy-kernel-contract
2. cy-proto
3. cy-manifest

4. cy-artifact-transfer       -> cy-kernel-contract, cy-manifest
5. cy-execution-fabric        -> cy-kernel-contract, cy-manifest, cy-proto
6. cy-workspace-fabric        -> cy-proto
```

The first three packages are independent leaves and may be published in any
order. The numbered order above is a conservative serial order. After every
successful publication, wait until the registry index is queryable before
starting a dependent publication. `cy-artifact-transfer` should be published
before `cy-execution-fabric` because the latter uses it in dev-dependencies.

These are only normal/build edges:

| Package | Public normal/build dependencies |
| --- | --- |
| `cy-kernel-contract` | none |
| `cy-manifest` | none |
| `cy-proto` | none |
| `cy-artifact-transfer` | `cy-kernel-contract`, `cy-manifest` |
| `cy-execution-fabric` | `cy-kernel-contract`, `cy-manifest`, `cy-proto` |
| `cy-workspace-fabric` | `cy-proto` |

## Verification procedure

From a clean checkout, first verify the graph and package contents:

```bash
cargo metadata --format-version 1 --locked --all-features
python tooling/ci/check-license-metadata.py
python tooling/ci/check-license-boundary.py
bash tooling/ci/check-public-packages.sh
bash tooling/acceptance/licensing-boundary/run.sh
```

Before the first publication, the package verifier uses temporary
`[patch.crates-io]` mappings to checked-out public packages. This proves the
package contents and local compilation but does not make an unpublished crate
available from crates.io.

After the prerequisite leaf versions are visible in the target registry, run
the dry-run for each package in the order above:

```bash
cargo publish --dry-run --allow-dirty -p cy-kernel-contract
cargo publish --dry-run --allow-dirty -p cy-proto
cargo publish --dry-run --allow-dirty -p cy-manifest
cargo publish --dry-run --allow-dirty -p cy-artifact-transfer
cargo publish --dry-run --allow-dirty -p cy-execution-fabric
cargo publish --dry-run --allow-dirty -p cy-workspace-fabric
```

`--allow-dirty` is shown only for local preflight use. A release must use a
clean, reviewed commit. No command in this document should be run with the
intention of uploading during this cleanup task.
