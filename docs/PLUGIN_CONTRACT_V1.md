# Platform Plugin Control Contract v1

Platform owns only the generic control-plane projection used to select and
supervise a Plugin. `Cyrene-Plugins-Official` owns `plugin.manifest.json` and all
capability payload contracts.

The Platform projection contains:

- Plugin identity and release version;
- opaque capability id and interface version;
- `INLINE`, `WORKER`, or `SERVICE` placement compatibility;
- optional immutable artifact reference;
- optional package launch command for a managed service;
- deterministic PluginSet resolution and lock evidence.

For package activation, the manifest's `runtime.launch` contains an executable
and arguments. `prepared-runtime` selects the executable produced by the
configured dependency preparer; any other value is a safe package-relative
path. Platform appends only `--capability`, `--interface-version`, and `--listen`
for the generic readiness handshake. The process publishes a bounded readiness
record containing the same capability/interface plus an opaque
`connection_ref`.

No capability method name, payload schema, Product state, language taxonomy,
provider type, or business policy is copied into the Platform model. A Product
uses the Plugin-owned SDK or wire contract to call `connection_ref` directly.
