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

Exchange integration grant behavior also has an executable Rust policy TCK in
`framework/crates/cy-workspace-product-contracts/tests/workspace_integration_policy_tck.rs`.
It loads the tracked Platform policy bytes and exercises the production policy
evaluator against a digest-pinned synthetic catalog containing the eight
Exchange operation IDs. Run it with:

```sh
cargo test --locked -p cy-workspace-product-contracts --test workspace_integration_policy_tck
```

The test also carries a colon-bearing provider-native authorization reference
through the route binding unchanged, uses UUID-shaped session/connection/
notification resources, and leaves notification feeds unscoped by resource ID.
Generic Product v2 keeps the JSON notification list; the SSE route stays on the
existing Control-specific HTTP stream and is not exposed as a generic query.
This scoped TCK proves policy accept/deny behavior; the release bundle/source
lock still requires validation against the final Exchange source commit.

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
