# Dependency Management & Dependabot Guide

This guide explains how to add dependencies, update lockfiles, and manage automated Dependabot updates.

---

## 1. Updating Python Dependencies with `uv`

When updating dependencies in `Cyrene-Platform` SDKs or isolated plugins:
```bash
# Add a dependency
uv add httpx

# Update lockfile reproducibly
uv lock

# Verify lockfile matches environment
uv sync --locked
```

---

## 2. Updating Rust Dependencies with `cargo`

```bash
# Check dependencies without updating lockfile
cargo check --locked

# Update a specific crate
cargo update -p serde

# Verify formatting and clippy
cargo fmt --check
cargo clippy --workspace --all-targets
```

---

## 3. Dependabot Schedule & Grouping

- Dependabot is configured to check for updates **weekly**.
- Routine patch and minor updates are grouped to prevent PR noise.
- Security alerts trigger immediate standalone updates.
---

<!-- Chinese Translation / 中文翻译 -->

# 依赖管理与 Dependabot 指南

本指南说明如何添加依赖、更新 lockfile，以及管理 Dependabot 自动更新。

---

## 1. 使用 `uv` 更新 Python 依赖

更新 `Cyrene-Platform` SDK 或隔离 plugin 的依赖时：

```bash
# 添加依赖
uv add httpx

# 以可复现方式更新 lockfile
uv lock

# 验证 lockfile 与环境一致
uv sync --locked
```

---

## 2. 使用 `cargo` 更新 Rust 依赖

```bash
# 检查依赖，但不更新 lockfile
cargo check --locked

# 更新指定 crate
cargo update -p serde

# 验证格式并运行 clippy
cargo fmt --check
cargo clippy --workspace --all-targets
```

---

## 3. Dependabot 的检查周期与分组

- Dependabot 配置为**每周**检查更新。
- 常规 patch 和 minor 更新会分组，以减少 PR 数量。
- 安全告警会立即触发独立更新。
