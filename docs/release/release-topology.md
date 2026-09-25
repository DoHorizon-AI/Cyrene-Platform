# Platform Release Topology

A Platform release is a component release from the protected `main` branch.
Each published artifact is tied to the exact release commit and uses the
repository SemVer. Python, Rust, and native artifacts may have different
registries, but they share the same reviewed Platform source boundary.

Cross-repository compatibility locks and end-user distributions consume those
artifacts externally. Platform release automation must not discover sibling
repositories or encode their versions, profiles, or deployment state.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 发布拓扑

Platform 发布是基于受保护 `main` 分支的组件发布。每个已发布制品都绑定到精确的发布提交，并使用仓库 SemVer。Python、Rust 和原生制品可以发布到不同 registry，但都来自同一份经过审查的 Platform 源码边界。

跨仓兼容性 lock 和面向最终用户的 distribution 在仓库外部消费这些制品。Platform 发布自动化不得发现同级仓库，也不得编码它们的版本、profile 或部署状态。
