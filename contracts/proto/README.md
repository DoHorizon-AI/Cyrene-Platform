# Proto contract modules

`cyrene/core/v1/` is the Core RPC module root. Every Core v1 file uses the
`cyrene.core.v1` package and is owned by the Buf lint/breaking/descriptor
pipeline. The Rust `cy-proto` crate uses the same module root with vendored
protoc and tonic-build to generate bindings into `OUT_DIR`.

The `plugin/v1/` module remains a separate local stdio protocol. Its imports
are module-relative, while its `cy.plugin.v1` package and wire semantics stay
unchanged.
