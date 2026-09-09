# Your First Platform Development Task

Use this workflow for a change owned by Cyrene-Platform.

## 1. Confirm ownership

Read [Where Does My Code Go?](03-where-does-my-code-go.md) and
[`repository-policy.yaml`](../../repository-policy.yaml). Platform changes must
be generic contracts, Kernel/runtime mechanisms, generic adapters, SDK
primitives, or repository-local tooling. Product behavior and concrete plugin
implementations have separate owners.

## 2. Create a topic branch

```bash
git switch -c feat/your-feature-name
```

## 3. Verify the change

```bash
python tooling/ci/verify.py --scope all-light
cargo fmt --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The hosted Azure pipeline is the remote source gate. Record local and hosted
results separately, then merge through the protected integration branch.
