# Platform Versioning and Compatibility

Cyrene-Platform uses repository-scoped Semantic Versioning. A breaking public
contract or SDK change increments the major version, a backward-compatible
feature increments the minor version, and a compatible fix increments the
patch version.

Capability interface major versions and schema versions remain explicit in
the contract itself. Their compatibility does not imply that a consumer or
plugin implementation is ready. Product, plugin, and distribution versions are
independent and are recorded by their owners.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 版本与兼容性

Cyrene-Platform 使用仓库级 Semantic Versioning。破坏性公开契约或 SDK 变更递增 major 版本；向后兼容的功能递增 minor 版本；兼容性修复递增 patch 版本。

Capability interface major 版本和 schema 版本会在契约中明确指定。它们彼此兼容，并不代表 consumer 或 plugin 实现已经可以使用。Product、plugin 和 distribution 使用独立版本，并由各自所有者记录。
