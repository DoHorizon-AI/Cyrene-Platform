# Kernel Semantic v1 fixture manifest

`manifest.json` binds the frozen, source-info-free Protobuf descriptor to the
semantic revision and shared TCK directory. The digest is checked by the
architecture-governance gate. Pull requests are additionally compared with
the target branch descriptor, so editing the local digest cannot hide a wire
breaking change.
---

<!-- Chinese Translation / 中文翻译 -->

# Kernel Semantic v1 Fixture Manifest

`manifest.json` 将冻结的、不含 source-info 的 Protobuf descriptor 与 semantic revision 及共享 TCK 目录绑定。architecture-governance gate 会校验其 digest。Pull request 也会与目标分支 descriptor 比较，因此仅修改本地 digest 无法掩盖 wire breaking change。
