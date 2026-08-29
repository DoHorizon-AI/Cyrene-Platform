# Core v1 wire fixtures

These fixtures are canonical protobuf wire bytes, represented as lowercase
hex. P1 verifies them with the Rust Core bindings; Kotlin, Python, and
TypeScript consumers can reuse the same wire values when their SDK stages land.
`manifest.json` records the SHA-256 of each wire value and the Core v1
descriptor digest used to produce it.

The fixture set is intentionally limited to contract compatibility. It does
not imply that the Node Agent opens a network listener or starts a plugin.
