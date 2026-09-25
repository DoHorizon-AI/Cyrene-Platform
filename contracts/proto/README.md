# Proto contract modules

`cyrene/core/v1/` is the Core RPC module root. Every Core v1 file uses the
`cyrene.core.v1` package and is owned by the Buf lint/breaking/descriptor
pipeline. The Rust `cy-proto` crate uses the same module root with vendored
protoc and tonic-build to generate bindings into `OUT_DIR`.

Platform owns only Kernel, node, hardware-adapter, sandbox-adapter, provider
lifecycle, semantic, and workspace-fabric control protocols. Capability
payloads and direct endpoint protocols live in the implementing Plugin or
Product repository. Adding a business method does not change this tree.

Core service responses intentionally use the typed domain messages from the
blueprint, and `Connect` intentionally streams the node/control envelopes;
Buf's generic RPC Request/Response naming rules are excluded for that reason.
---

<!-- Chinese Translation / 中文翻译 -->

# Proto 契约模块

`cyrene/core/v1/` 是 Core RPC module 根目录。每个 Core v1 文件都使用 `cyrene.core.v1` package，并由 Buf lint/breaking/descriptor pipeline 管理。Rust `cy-proto` crate 使用相同 module 根目录，通过 vendored protoc 和 tonic-build 将 binding 生成到 `OUT_DIR`。

Platform 只拥有 Kernel、node、hardware-adapter、sandbox-adapter、provider lifecycle、semantic 和 workspace-fabric control 协议。Capability payload 和 direct endpoint 协议由实现它们的 Plugin 或 Product 仓库拥有。新增业务方法不会修改此目录。

Core service response 有意使用 blueprint 中的类型化 domain message；`Connect` 也有意以 stream 传输 node/control envelope。基于这些设计，Buf 通用 RPC Request/Response 命名规则不适用于该接口。
