# Vendored Google RPC status contract

`status.proto` is the locally controlled copy of the standard Google RPC
status message from googleapis/googleapis. Its package and wire shape are kept
unchanged so Core v1 descriptors are reproducible without a developer-local
Proto installation.

The file is intentionally tracked with the contract module and must be
updated together with its source/license record when the upstream definition
changes.
