# Local Verification Guide

Before pushing code or opening a pull request, run the single-entry local verification tool.

---

## 1. Quick Verification (One Command)

In `Cyrene-Platform`, run:
```bash
python tooling/ci/verify.py
```

This runs all lightweight Tier 0 and Tier 1 checks:
1. Documentation link and structure validation (`tooling/docs/validate_docs.py`).
2. Architecture and service boundary governance (`tooling/ci/check_service_boundaries.py`).
3. Python SDK unit test suites (`cyrene_preflight` and `cyrene_artifacts`).
4. Platform-local manifest and repository-policy validation.

---

## 2. Scoped Verification Commands

You can also target specific subsystems:

```bash
# Validate documentation links and indexes only
python tooling/ci/verify.py --scope docs

# Run architecture boundary and governance checks only
python tooling/ci/verify.py --scope governance

# Run Python SDK tests only
python tooling/ci/verify.py --scope python

# Run Rust formatting and tests only
python tooling/ci/verify.py --scope rust
```
---

<!-- Chinese Translation / 中文翻译 -->

# 本地验证指南

推送代码或创建 pull request 前，请运行统一的本地验证工具。

---

## 1. 快速验证（一条命令）

在 `Cyrene-Platform` 中运行：

```bash
python tooling/ci/verify.py
```

这会运行 Tier 0 和 Tier 1 的所有轻量检查：
1. 文档链接与结构校验（`tooling/docs/validate_docs.py`）。
2. 架构和 service 边界治理（`tooling/ci/check_service_boundaries.py`）。
3. Python SDK 单元测试套件（`cyrene_preflight` 和 `cyrene_artifacts`）。
4. Platform 本地 manifest 与 repository-policy 校验。

---

## 2. 指定范围的验证命令

也可以只验证特定子系统：

```bash
# 仅校验文档链接和索引
python tooling/ci/verify.py --scope docs

# 仅运行架构边界和治理检查
python tooling/ci/verify.py --scope governance

# 仅运行 Python SDK 测试
python tooling/ci/verify.py --scope python

# 仅运行 Rust 格式检查和测试
python tooling/ci/verify.py --scope rust
```
