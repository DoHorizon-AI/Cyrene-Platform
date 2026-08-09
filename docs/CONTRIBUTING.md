# Contributing during the private redesign

The core is currently developed privately. Keep changes inside one of the public
boundaries: `kernel/`, `framework/`, `contracts/`, or `examples/`.

Do not add first-party service behavior to this repository. Catalyst, Yield,
Reactor, Exchange, Navigator, Echo, vendor implementations, and enterprise
bundles belong in the advanced-services repository.

Before handing off a core change, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --offline
python framework/tooling/validate_advanced_service.py \
  --schema contracts/schemas/advanced-service.schema.json \
  examples/advanced-service/service.json
```

Contract changes require a versioning decision and a compatibility test before
an advanced service may consume them.
