# Core v1 wire fixtures

These fixtures are canonical protobuf wire bytes, represented as lowercase
hex. P1 verifies them with the Rust Core bindings; Kotlin, Python, and
TypeScript consumers can reuse the same wire values when their SDK stages land.
`manifest.json` records the SHA-256 of each wire value and the Core v1
descriptor digest used to produce it.

The fixture set is intentionally limited to contract compatibility. It does
not imply that the Node Agent opens a network listener or starts a plugin.
---

<!-- Chinese Translation / 中文翻译 -->

# Core v1 线协议 Fixture

这些 fixture 是规范的 protobuf wire 字节，以小写十六进制表示。P1 使用 Rust Core binding 校验它们；当 Kotlin、Python 和 TypeScript consumer 的 SDK 阶段就绪后，可以复用相同的 wire value。`manifest.json` 记录每个 wire value 的 SHA-256，以及生成它们时使用的 Core v1 descriptor digest。

Fixture 集合仅用于契约兼容性，不表示 Node Agent 会启动网络 listener 或 plugin。
