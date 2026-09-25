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
---

<!-- Chinese Translation / 中文翻译 -->

# 你的第一个 Platform 开发任务

当改动由 Cyrene-Platform 负责时，按以下流程进行。

## 1. 确认归属

阅读[代码应放在哪里？](03-where-does-my-code-go.md)和 `repository-policy.yaml`。Platform 改动必须属于通用契约、Kernel/runtime 机制、通用 adapter、SDK 基元或仓库本地工具。Product 行为和具体 Plugin 实现由其他所有者负责。

## 2. 创建 topic 分支

```bash
git switch -c feat/your-feature-name
```

## 3. 验证改动

```bash
python tooling/ci/verify.py --scope all-light
cargo fmt --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

托管 Azure pipeline 是远端源码 gate。请分别记录本地和托管验证结果，再通过受保护的集成分支合并。
