# manifests Directory Guide | contracts/schemas/manifests 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `artifact_manifest.schema.json` | `MIGRATING_COMPATIBILITY` v0 AI artifact projection. | `MIGRATING_COMPATIBILITY` v0 AI 产物投影。 |
| `artifact_ref.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `checkpoint_metadata.schema.json` | `MIGRATING_COMPATIBILITY` v0 Product checkpoint projection. | `MIGRATING_COMPATIBILITY` v0 Product checkpoint 投影。 |
| `hardware_manifest.schema.json` | `MIGRATING_COMPATIBILITY` v0 AI hardware projection. | `MIGRATING_COMPATIBILITY` v0 AI 硬件投影。 |
| `model_manifest.schema.json` | `MIGRATING_COMPATIBILITY` v0 model-analysis projection. | `MIGRATING_COMPATIBILITY` v0 模型分析投影。 |
| `model_version.schema.json` | Immutable FULL_MODEL or one-base-plus-one-LoRA composition consumed by Products. | 由 Product 消费的不可变 FULL_MODEL 或单 base 加单 LoRA 组合。 |
| `portable_directory_manifest.schema.json` | Cross-language V2 directory artifact index; paths and raw CAS digests are validated by Rust/Python implementations. | 跨语言 V2 目录 Artifact 索引；路径和 raw CAS digest 由 Rust/Python 实现校验。 |
| `runtime_manifest.schema.json` | `MIGRATING_COMPATIBILITY` v0 AI runtime projection. | `MIGRATING_COMPATIBILITY` v0 AI 运行时投影。 |
| `training_revision.schema.json` | `MIGRATING_COMPATIBILITY` v0 training lifecycle projection. | `MIGRATING_COMPATIBILITY` v0 训练生命周期投影。 |
| `validation_result.schema.json` | `MIGRATING_COMPATIBILITY` v0 validation projection. | `MIGRATING_COMPATIBILITY` v0 验证投影。 |
| `why_report.schema.json` | `MIGRATING_COMPATIBILITY` v0 planning report. | `MIGRATING_COMPATIBILITY` v0 规划报告。 |
| `workload_request.schema.json` | `MIGRATING_COMPATIBILITY` v0 training and serving request. | `MIGRATING_COMPATIBILITY` v0 训练与服务请求。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
