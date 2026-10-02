# Workspace Product API v2 TCK

This table is the generic cross-language test matrix for the Product v2
invocation, source-pinned owner catalog, and Platform-owned authorization
policy. `scenarios.tsv` defines the required input class and
observable result. It contains no Product operation enum: owner and operation
selection resolves from a reviewed release catalog, and only operations with a
separate Platform policy grant can reach an adapter.

The v1 `product-projections.tsv` remains the initial migration
seed for the first 13 projected Product operations. Runtime routing does not
read or compile its legacy wire-operation identifiers. Adding an operation
requires an owner catalog/OpenAPI source pin and an explicit Platform policy
decision; adding a compatible owner or operation does not require changing
core Rust or protobuf source.

Release inputs are assembled by
`tooling/workspace-product-contract-bundle/build_v2_bundle.py` from
exact Git commit objects. The checked-in compatibility lock is the Platform
authority used by build.rs; the build context cannot supply or redefine pins.
The owner catalog, bundle manifest, policy, and lock shapes are published in
`contracts/schemas/`.

Run the bundle source and lock checks from the Platform repository:

```sh
python3 tooling/workspace-product-contract-bundle/build_v2_bundle.py \
  --source catalyst=/path/to/Cyrene-Catalyst --commit catalyst=<full-sha> \
  --source echo=/path/to/Cyrene-Echo --commit echo=<full-sha> \
  --source exchange=/path/to/Cyrene-Exchange --commit exchange=<full-sha> \
  --source navigator=/path/to/Cyrene-Navigator --commit navigator=<full-sha> \
  --source reactor=/path/to/Cyrene-Reactor --commit reactor=<full-sha> \
  --source yield=/path/to/Cyrene-Yield --commit yield=<full-sha> \
  --output /tmp/workspace-product-v2-bundle \
  --verify-lock tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json
```
