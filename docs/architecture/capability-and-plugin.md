# Capabilities and Plugins

A Plugin owns one or more versioned capability contracts and an independently
runnable implementation. Its repository manifest is the authority for identity,
release, methods, payload schemas, protocol, and runtime launcher.

Platform performs generic discovery, compatibility selection, package
verification, process supervision, and endpoint publication. Products call the
selected Plugin endpoint directly through a Plugin-owned client contract.
Platform never imports the implementation or handles capability payload bytes.
---

<!-- Chinese Translation / 中文翻译 -->

# Capabilities 与 Plugins

一个 Plugin 拥有一个或多个带版本的 capability 契约，以及可独立运行的实现。其仓库清单是身份、发布版本、方法、负载 schema、协议和运行时启动器的权威来源。

Platform 负责通用发现、兼容性选择、软件包验证、进程监管和端点发布。Products 通过由 Plugin 所有的客户端契约，直接调用选中的 Plugin 端点。Platform 不导入实现，也不处理 capability 负载字节。
