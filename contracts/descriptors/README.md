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
---

<!-- Chinese Translation / 中文翻译 -->

# 契约 Descriptor 基线

`cyrene-core-v1.pb` 是 Core v1 module 的已检入 descriptor 基线，包含 imports 和源文件信息。其 SHA-256 记录在 Core v1 fixture manifest 中。任何契约变更都必须重新生成 descriptor，并在替换基线前通过 Buf breaking check。

`cyrene-kernel-semantic-v1.pb` 保留源文件信息，便于审查；`cyrene-kernel-semantic-v1-stable.pb` 是确定性冻结 v1 基线。其 SHA-256 由 `contracts/fixtures/semantic/v1/manifest.json` 绑定。Pull request 期间，治理 gate 使用目标分支的 stable descriptor 作为 breaking baseline，而不是只信任 PR 修改过的文件。
