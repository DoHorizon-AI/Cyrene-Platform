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
