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
| `artifact_manifest.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `artifact_ref.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `checkpoint_metadata.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `hardware_manifest.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `model_manifest.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `portable_directory_manifest.schema.json` | Cross-language V2 directory artifact index; paths and raw CAS digests are validated by Rust/Python implementations. | 跨语言 V2 目录 Artifact 索引；路径和 raw CAS digest 由 Rust/Python 实现校验。 |
| `runtime_manifest.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `training_revision.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `validation_result.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `why_report.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `workload_request.schema.json` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
