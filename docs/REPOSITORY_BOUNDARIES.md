# Repository boundaries / 仓库边界

Cyrene-Platform owns Product-neutral mechanisms: Kernel process and resource
lifecycle, generic execution control, ArtifactRef and transfer, Plugin package
installation and supervision, compatibility resolution, and opaque endpoint
grants. It must build and test independently from every Product and Plugin
repository.

Cyrene-Platform 只持有与产品无关的机制：Kernel 进程与资源生命周期、通用执行控制、
ArtifactRef 与传输、Plugin 包安装与监督、兼容性解析，以及不透明端点授权。它必须能够
脱离所有 Product 与 Plugin 仓库独立构建和测试。

Products own user intent, domain state, lifecycle policy, retry policy,
persistence, billing, routing decisions, and user-facing workflows. Plugins own
their callable payload contracts, methods, SDKs, implementation entrypoints,
runtime adapters, configuration, and direct endpoint protocols.

Product 持有用户意图、领域状态、生命周期策略、重试策略、持久化、计费、路由决策和
面向用户的工作流。Plugin 持有自身的调用载荷契约、方法、SDK、实现入口、运行时适配、
配置和直接端点协议。

## Change rule / 修改规则

Adding or changing a Product capability must not require a Platform source
change. Platform may validate generic identity, version, execution mode,
permissions, health, and package-owned launch metadata. It must not parse,
proxy, transform, route, or persist a capability's business payload.

新增或修改 Product capability 不得要求修改 Platform 源码。Platform 可以校验通用的
身份、版本、执行模式、权限、健康状态和包自有启动元数据；不得解析、代理、转换、路由
或持久化 capability 的业务载荷。

Cross-repository workflow ownership, accepted revisions, and remediation status
are recorded in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace). Historical
reports are evidence snapshots and do not override current contracts or live Git
state.

跨仓库工作流归属、已接受版本和整改状态记录在
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace)。历史报告仅是
证据快照，不能覆盖当前契约或实时 Git 状态。
