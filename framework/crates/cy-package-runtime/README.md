# cy-package-runtime

`cy-package-runtime` is the node-local control-plane owner for verified package
installation, locked dependency preparation, binding activation, process
supervision, status, upgrade, rollback, and cleanup.

For a running service binding it returns an opaque `connection_ref`. The
Product opens that endpoint with the Plugin-owned versioned protocol. This
crate has no invoke, stream, subscribe, request/response payload, or domain
error API.

The current built-in launcher starts a Python package that carries the
Plugins-owned `cyrene_plugin_runtime`. Adding a model, media processor, message
connector, or other Python capability changes the Plugin package and Product
adapter only; it does not require a Platform source change.
