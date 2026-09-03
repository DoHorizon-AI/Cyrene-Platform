# Contract descriptor baselines

`cyrene-core-v1.pb` is the checked-in descriptor baseline for the Core v1
module, including imports and source information. Its SHA-256 is recorded in
the Core v1 fixture manifest. Any contract change must regenerate the
descriptor and pass the Buf breaking check before the baseline is replaced.

`cyrene-kernel-semantic-v1.pb` preserves source information for review;
`cyrene-kernel-semantic-v1-stable.pb` is the deterministic frozen-v1 baseline.
Its SHA-256 is bound by `contracts/fixtures/semantic/v1/manifest.json`. During
pull requests, the governance gate uses the target branch stable descriptor as
the breaking baseline rather than trusting only files modified by the PR.
