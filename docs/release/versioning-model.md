# Platform Versioning and Compatibility

Cyrene-Platform uses repository-scoped Semantic Versioning. A breaking public
contract or SDK change increments the major version, a backward-compatible
feature increments the minor version, and a compatible fix increments the
patch version.

Capability interface major versions and schema versions remain explicit in
the contract itself. Their compatibility does not imply that a consumer or
plugin implementation is ready. Product, plugin, and distribution versions are
independent and are recorded by their owners.
