# Platform Tagging and Release Identity

Platform release tags use
`v<MAJOR>.<MINOR>.<PATCH>[-<PRERELEASE>]` and point to an exact commit on the
protected `main` branch. Published tags are immutable; a correction receives a
new patch version.

Validate a candidate tag with:

```bash
python tooling/release/validate_tag.py --repo-path . --tag v0.4.2
```

Create a tag only after the exact commit passes local release checks and the
required Azure source gates. The repository's release automation prepares a
draft release; it does not define tags for Products, plugins, or a combined
distribution.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform Tag 与发布身份

Platform 发布 tag 使用 `v<MAJOR>.<MINOR>.<PATCH>[-<PRERELEASE>]` 格式，并指向受保护 `main` 分支上的精确提交。已发布 tag 不可变；更正需发布新的 patch 版本。

使用以下命令校验候选 tag：

```bash
python tooling/release/validate_tag.py --repo-path . --tag v0.4.2
```

精确提交通过本地发布检查和 Azure 必需源码 gate 后，才能创建 tag。仓库发布自动化会准备 draft release，不会定义 Products、plugins 或组合 distribution 的 tag。
