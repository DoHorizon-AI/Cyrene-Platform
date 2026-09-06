# Proto contract modules

`cyrene/core/v1/` is the Core RPC module root. Every Core v1 file uses the
`cyrene.core.v1` package and is owned by the Buf lint/breaking/descriptor
pipeline. The Rust `cy-proto` crate uses the same module root with vendored
protoc and tonic-build to generate bindings into `OUT_DIR`.

The `plugin/v1/` module remains a separate local stdio protocol. Its imports
are module-relative, while its `cy.plugin.v1` package and wire semantics stay
unchanged.

`cyrene/capability/v1/` owns the language-neutral Product execution service.
Capability-specific payloads live under their own packages; the EXPERIMENTAL
`cyrene/message/connector/v1/` module defines typed message connector payloads
without declaring another gRPC service.

`cyrene/model/provider/v1/` owns EXPERIMENTAL typed payloads for the stateless
`chat_completion`, `chat_completion_v2`, and `embeddings` methods of
`model.provider.v1`. Interface version 1 remains text/embedding compatible;
interface version 2 adds function tools, tool-call history, indexed streamed
tool-call deltas, and provider-reported stream usage through additive fields.
It declares no gRPC service and contains no Product routing, memory, or
vector-storage policy.

Core service responses intentionally use the typed domain messages from the
blueprint, and `Connect` intentionally streams the node/control envelopes;
Buf's generic RPC Request/Response naming rules are excluded for that reason.
