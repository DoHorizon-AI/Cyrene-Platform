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
