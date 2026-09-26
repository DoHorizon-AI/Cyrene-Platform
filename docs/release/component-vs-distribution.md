# Platform Component Release Boundary

Cyrene-Platform publishes independently versioned component artifacts: Rust
binaries and libraries, and Python SDK distributions. Its
repository version and tags describe only these Platform-owned artifacts.

A multi-repository distribution combines independently released Platform,
Product, and plugin artifacts under an external immutable lock. Distribution
profiles, compatibility pins, installers, and deployment bundles are not owned
by Platform; their authority lives in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) or a
dedicated distribution repository.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 组件发布边界

Cyrene-Platform 发布独立版本化的组件制品：Rust 二进制文件与库，以及 Python SDK distribution。仓库版本号和 tag 只描述这些由 Platform 所有的制品。

多仓 distribution 会在外部不可变 lock 下组合独立发布的 Platform、Product 和 plugin 制品。distribution profile、兼容性 pin、安装器和部署 bundle 不属于 Platform；其权威归 Cyrene-Workspace 或专门的 distribution 仓库。
