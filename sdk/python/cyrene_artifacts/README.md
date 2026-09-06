# cyrene_artifacts Directory Guide | sdk/python/cyrene_artifacts 目录指南

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
| `pyproject.toml` | Supporting metadata or fixture. | 辅助元数据或 fixture。 |
| `src/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |
| `tests/` | Nested source or contract boundary; read its README next. | 嵌套源码或契约边界，下一步阅读其 README。 |

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。

## Portable directory artifacts | Portable 目录 Artifact

`publish_portable_directory()` publishes the public directory contract
`PortableDirectoryManifest` (version 2). Its canonical JCS identity contains
only strictly sorted logical POSIX-relative file paths, each raw blob
`sha256:` digest and byte size, plus the logical sum `size_bytes`. The
`ArtifactRef.digest` and `manifest_digest` both identify those canonical
manifest bytes; `ArtifactRef.size_bytes` is the logical sum and excludes the
manifest file itself. Provider metadata is stored in the separate
`ArtifactManifest` projection and never changes this identity.

`publish()` retains the legacy provider-private version 1 format for existing
callers. `resolve()` and `stage()` read both versions, while version 2 rejects
absolute paths, `.`/`..`, backslashes, Windows drive prefixes, controls,
symlinks, duplicate or prefix-conflicting paths, and ASCII case-insensitive
aliases at every directory prefix. V2 is Linux-first and does not put Unicode
case mapping or normalization into the identity key; a Windows materializer
must add its target-filesystem checks. A temporary directory is atomically
renamed only after every blob has been verified.

`publish_portable_directory()` 发布公共目录契约
`PortableDirectoryManifest`（version 2）。其 canonical JCS 身份只包含严格排序的
逻辑 POSIX 相对文件路径、每个 raw blob 的 `sha256:` 摘要和字节数，以及逻辑总和
`size_bytes`。`ArtifactRef.digest` 与 `manifest_digest` 都指向该 canonical
manifest；`ArtifactRef.size_bytes` 是逻辑总和，不含 manifest 文件本身。供应商
元数据属于独立的 `ArtifactManifest` 投影，不改变该身份。

`publish()` 继续保留旧的 provider-private V1 格式；`resolve()` 和 `stage()`
同时读取两个版本。V2 会拒绝绝对路径、`.`/`..`、反斜杠、Windows drive 前缀、
控制字符、符号链接、重复或前缀冲突路径，以及每个目录前缀上的 ASCII 大小写
别名。V2 以 Linux 为优先目标，不把 Unicode 大小写映射或规范化写入身份键；
Windows materializer 仍须增加目标文件系统检查。只有所有 blob 校验完成后，临时
目录才会原子改名为目标目录。
