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
---

<!-- Chinese Translation / 中文翻译 -->

# 公共 Package 发布顺序

本文从 release engineering 角度说明公开 Rust package 的依赖 DAG，不会实际发布 package。每次版本发布前都必须使用 Cargo metadata 重新检查依赖图。

## 当前依赖顺序

先发布叶子契约，等 registry index 能查询到对应版本后，再发布依赖它们的 package：

\`\`\`text
1. cy-kernel-contract
2. cy-proto
3. cy-manifest

4. cy-artifact-transfer       -> cy-kernel-contract, cy-manifest
5. cy-execution-fabric        -> cy-kernel-contract, cy-manifest, cy-proto
6. cy-workspace-fabric        -> cy-proto
\`\`\`

前三个 package 是相互独立的叶子，可以按任何顺序发布。上面的编号顺序是保守的串行顺序。每个 package 成功发布后，都要等 registry index 可查询后再开始发布依赖它的 package。由于 \`cy-execution-fabric\` 在 dev-dependency 中使用 \`cy-artifact-transfer\`，后者应先于前者发布。

以下只列普通/build dependency：

| Package | 公开的普通/build 依赖 |
|---|---|
| \`cy-kernel-contract\` | 无 |
| \`cy-manifest\` | 无 |
| \`cy-proto\` | 无 |
| \`cy-artifact-transfer\` | \`cy-kernel-contract\`、\`cy-manifest\` |
| \`cy-execution-fabric\` | \`cy-kernel-contract\`、\`cy-manifest\`、\`cy-proto\` |
| \`cy-workspace-fabric\` | \`cy-proto\` |

## 验证流程

从干净检出开始，先验证依赖图和 package 内容：

\`\`\`bash
cargo metadata --format-version 1 --locked --all-features
python tooling/ci/check-license-metadata.py
python tooling/ci/check-license-boundary.py
bash tooling/ci/check-public-packages.sh
bash tooling/acceptance/licensing-boundary/run.sh
\`\`\`

首次发布前，package verifier 使用临时 \`[patch.crates-io]\` 映射到已检出的公开 package。这可以验证 package 内容和本地编译，但不会让尚未发布的 crate 能从 crates.io 下载。

在必要的叶子版本已显示于目标 registry 后，按上述顺序对每个 package 执行 dry-run：

\`\`\`bash
cargo publish --dry-run --allow-dirty -p cy-kernel-contract
cargo publish --dry-run --allow-dirty -p cy-proto
cargo publish --dry-run --allow-dirty -p cy-manifest
cargo publish --dry-run --allow-dirty -p cy-artifact-transfer
cargo publish --dry-run --allow-dirty -p cy-execution-fabric
cargo publish --dry-run --allow-dirty -p cy-workspace-fabric
\`\`\`

这里只为本地 preflight 展示 \`--allow-dirty\)。真实发布必须使用干净且经过审查的 commit。本文档中的任何命令都不应在本次清理任务中以实际上传为目的运行。
