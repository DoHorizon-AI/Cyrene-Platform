# Platform Artifact Destinations

Platform release automation may publish only artifacts built from this
repository: native binaries and bundles, Python packages, Rust crates, and Platform-owned
container images. Every published artifact must
be traceable to the exact source commit and immutable digest.

Registry configuration and credentials are release-environment concerns.
Product images, plugin bundles, combined installers, download portals, and
private deployment assets are published by their owning repositories.
---

<!-- Chinese Translation / 中文翻译 -->

# Platform 制品发布目标

Platform 发布自动化只能发布由本仓库构建的制品：原生二进制文件与 bundle、Python package、Rust crate，以及 Platform 所有的容器镜像。每个已发布制品都必须能追溯到精确的源代码提交和不可变摘要。

Registry 配置和凭证属于发布环境。Product 镜像、plugin bundle、组合安装器、下载门户和私有部署资源由各自所有者发布。
