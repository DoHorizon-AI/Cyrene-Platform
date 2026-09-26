# cy-proto

This crate owns the Rust client/server API generated from the
`contracts/proto/cyrene/core/v1/` module. The public package is
`cyrene::core::v1`; `protoc` is supplied by `protoc-bin-vendored`, so workspace
builds do not require a system install.

The `.proto` files under `proto/` and the core fixture manifest under
`tests/fixtures/` are package-local mirrors of the canonical
`contracts/proto/` and `contracts/fixtures/` inputs. Keeping the inputs in the
crate makes `cargo package` and independent consumers work outside the
monorepo; CI checks that the mirrors remain byte-for-byte synchronized.
---

<!-- Chinese Translation / 中文翻译 -->

# cy-proto

此 crate 拥有从 `contracts/proto/cyrene/core/v1/` module 生成的 Rust client/server API。公开 package 为 `cyrene::core::v1`；protoc 由 `protoc-bin-vendored` 提供，所以 workspace build 不要求系统安装 protoc。

`proto/` 下的 `.proto` 文件以及 `tests/fixtures/` 下的 Core fixture manifest，是规范 `contracts/proto/` 与 `contracts/fixtures/` 输入的 crate 内镜像。将输入保留在 crate 中，可确保 `cargo package` 和独立 consumer 在 monorepo 外也能工作；CI 会检查镜像是否仍与来源逐字节一致。
