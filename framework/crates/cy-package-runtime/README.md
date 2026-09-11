# cy-package-runtime

`cy-package-runtime` is the node-local control-plane owner for verified package
installation, locked dependency preparation, binding activation, process
supervision, status, upgrade, rollback, and cleanup.

A package supplies a language-neutral launch command. A configured external
adapter prepares language dependencies through
`cyrene.package-dependency-preparer.v1`; Platform validates its bounded JSON
evidence without knowing Python, Java, .NET, or another toolchain. The
supervisor then executes a verified package-relative binary or the prepared
runtime executable, appends the generic readiness arguments, and returns an
opaque `connection_ref`. Language adapters and capability protocols stay in the
package repository.

The Product opens `connection_ref` using the Plugin-owned versioned client.
This crate has no invoke, stream, subscribe, method, request/response payload,
or domain-error API.
