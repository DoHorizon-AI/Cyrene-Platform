# CYRENE Platform Component Boundary v1

Status: **Normative**

Platform owns Kernel semantics, generic control-plane policy, verified process
supervision, Artifact Plane primitives, and Plugin lifecycle/discovery. Products
own application state and workflow. Plugin repositories own reusable capability
payload contracts, launchers, SDKs, implementations, and TCKs.

Dependency direction is:

```text
Product ──control request──► Platform ──authority action──► Kernel
   │                            │
   └──direct business call──► Plugin endpoint
                                ▲
                     Platform starts/observes process only
```

A Product or Plugin never imports Kernel private crates or calls a private
Kernel socket. Platform never imports Product/Plugin implementation code and
never handles business request or response payloads. Vendor hardware code runs
in an authenticated Adapter Host and is reduced to generic resource facts and
actions.

A new Platform primitive requires two independent consumers or a Kernel-level
invariant. Single-consumer adapters, Product manifests, deployment assets,
ecosystem catalogs, and migration snapshots stay in their owner repository.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Platform 组件边界 v1

状态：**规范性文件**

Platform 拥有 Kernel 语义、通用控制面策略、已验证的进程监管、Artifact Plane 基元，以及 Plugin 生命周期/发现机制。Products 拥有应用状态和工作流。Plugin 仓库拥有可复用的 capability 负载契约、启动器、SDK、实现和 TCK。

依赖方向如下：

```text
Product ──control request──► Platform ──authority action──► Kernel
   │                            │
   └──direct business call──► Plugin endpoint
                                ▲
                     Platform only starts/observes process
```

Product 或 Plugin 不得导入 Kernel 私有 crate，也不得调用私有 Kernel socket。Platform 不导入 Product/Plugin 实现代码，也不处理业务 request 或 response 负载。厂商硬件代码在经过认证的 Adapter Host 中运行，并被简化为通用资源事实和操作。

新增 Platform 基元必须服务于两个相互独立的 consumer，或维护一项 Kernel 级不变量。单一 consumer 的 adapter、Product manifest、部署资源、生态目录和迁移快照都应留在其所有者仓库。
