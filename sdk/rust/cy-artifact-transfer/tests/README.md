# `cy-artifact-transfer` tests

This directory verifies provider-neutral Artifact transfer and V2 portable
directory materialization. The Rust API consumes an authorized `ArtifactRef`,
manifest bytes, and a caller-owned digest source; it does not own provider
locators or CAS catalogs. Directory publication is Linux V1 only and requires
a trusted owner-controlled staging root; the tests do not claim arbitrary
same-UID race protection.
The immediate destination parent must be created by the caller, which also
owns durability for higher-level ancestors.

本目录验证 provider-neutral Artifact 传输与 V2 可移植目录 materialize。Rust API
只消费已授权的 `ArtifactRef`、manifest bytes 和调用方提供的 digest source，不拥有
provider locator 或 CAS catalog。
目录发布当前仅支持 Linux V1，并要求受信 owner 控制的 staging 根目录；测试不宣称
可以防护任意同 UID 进程造成的竞态。
目标的直接父目录必须由调用方预先创建并负责更高层目录的持久化。

| File | Responsibility |
| --- | --- |
| `portable_directory_materialize.rs` | Real-file, cross-SDK, integrity, symlink, conflict, and atomic-publication tests. |
| `fixtures/publish_portable_directory.py` | Test-only adapter that calls Python `LocalArtifactProvider.publish_portable_directory` and exports a neutral manifest/blob view. |
| `fixtures/README.md` | Documents the test-only Python-to-Rust fixture boundary. |

Run the focused acceptance test with:

```bash
cargo test --locked -p cy-artifact-transfer --test portable_directory_materialize
```

The cross-SDK case requires `python3` on `PATH`; set `CYRENE_PYTHON` to an
explicit interpreter when needed. The fixture adapter is not a production
dependency and its provider-private path access must remain confined here.
The materializer accepts ordinary pretty JSON; only the Python provider's
persisted CAS manifest is required to use canonical raw bytes by its own
resolver contract.

运行 focused acceptance test：

```bash
cargo test --locked -p cy-artifact-transfer --test portable_directory_materialize
```

跨 SDK 测试要求 `PATH` 中有 `python3`；必要时通过 `CYRENE_PYTHON` 指定解释器。
fixture adapter 不是生产依赖，provider 私有路径访问必须保持在本目录内。
materializer 接受普通 pretty JSON；只有 Python provider 自己的 resolver 契约要求持久化
CAS manifest 使用 canonical raw bytes。
---

<!-- Chinese Translation / 中文翻译 -->

# `cy-artifact-transfer` 测试

本目录验证与 Provider 无关的 Artifact transfer 和 V2 portable directory materialize。Rust API 消费已授权的 `ArtifactRef`、manifest bytes 和调用方提供的 digest source；它不拥有 provider locator 或 CAS catalog。目录发布当前仅支持 Linux V1，且要求可信、由 owner 控制的 staging root；测试不宣称可以防护任意同 UID race。目标的直接父目录必须由调用方创建，调用方也负责更高层 ancestor 的持久性。

本目录验证与 provider-neutral Artifact transfer 和 V2 portable directory materialize。Rust API 只消费已授权的 `ArtifactRef`、manifest bytes 和调用方提供的 digest source，不拥有 provider locator 或 CAS catalog。
目录发布目前仅支持 Linux V1，要求 trusted owner 控制 staging root；测试不声称可防护任意 same-UID race。目标的直接父目录必须由调用方创建，更高层目录的 durability 也由调用方负责。

| 文件 | 职责 |
|---|---|
| `portable_directory_materialize.rs` | 真实文件、跨 SDK、完整性、符号链接、冲突和原子发布测试。 |
| `fixtures/publish_portable_directory.py` | 仅供测试的 adapter，调用 Python `LocalArtifactProvider.publish_portable_directory`，并导出中性 manifest/blob 视图。 |
| `fixtures/README.md` | 说明仅供测试使用的 Python-to-Rust fixture 边界。 |

运行针对性的 acceptance test：

```bash
cargo test --locked -p cy-artifact-transfer --test portable_directory_materialize
```

跨 SDK 场景要求 `PATH` 中存在 `python3`；必要时通过 `CYRENE_PYTHON` 指定解释器。Fixture adapter 不是生产依赖，其对 provider-private path 的访问必须限制在本目录。Materializer 接受普通 pretty JSON；只有 Python provider 自己的 resolver contract 要求持久化 CAS manifest 使用 canonical raw bytes。
