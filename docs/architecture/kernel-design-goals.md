# CYRENE Kernel Design Goals

**Status:** long-lived design guidance; not a versioned wire contract or an
implementation checklist.

This document records the direction and invariants that guide Kernel evolution.
The exact v1 meanings, fields, error handling, and compatibility rules are
defined only by the [Kernel Semantic Contract v1](../contracts/kernel-semantic-contract-v1.md).
Work that is currently expected to be implemented or validated is tracked in
the [Kernel Execution Goals](../operations/kernel-execution-goals.md).

## Purpose and boundary

CYRENE Kernel is the local authority for a node's resources and their safe use.
It owns identity, resource observation, lease authority, fencing, local worker
lifecycle, local control-plane access, and the facts emitted about those
changes. It does not become a global scheduler, a vendor SDK, a device driver,
or a product/business layer.

The architectural repository boundary is the
[Platform clean boundary](../governance/platform-clean-boundary.md). The older
[Engine Architecture Blueprint](../CYRENE_ENGINE_ARCHITECTURE_BLUEPRINT.md) is
retained only as historical context.
Platform and resource-specific isolation stay in external adapters or services;
the Kernel keeps the common semantic authority.

## Design principles

1. **Semantic model before projection.** Kernel concepts are defined once in
   the semantic model. gRPC, UDS, C ABI, JVM, Kotlin, or other transport and
   language views are projections, not independent sources of truth.
2. **Local authority is explicit.** A node can authorize only resources and
   workers it owns. A resource must have one authoritative provider at a time,
   and every mutating action must be attributable to a principal.
3. **Leases carry authority and fencing.** A lease is finite, renewable, and
   associated with a fence. Stale authorities must be rejected rather than
   silently overriding a newer holder.
4. **Providers normalize heterogeneity.** Hardware, execution, network, and
   similar backends expose stable semantic capabilities and resources. Vendor
   details may be retained as provider metadata, but they must not leak into
   the Kernel's common authority model.
5. **Reconciliation repairs observed state.** Inventory reports what a
   provider sees; reconciliation decides whether it matches desired state and
   produces the necessary repair work. The two concerns must not be conflated.
6. **Workers are controlled local processes.** A worker has an owner, a
   generation, observable lifecycle, and a failure path. Kernel recovery may
   clean up or reconcile known work, but must not adopt an unverified foreign
   process.
7. **Control and data paths are distinct.** The Kernel authorizes and
   describes endpoints; it is not the high-throughput payload forwarding path.
   Endpoint exposure must retain ownership and authorization information.
8. **Events are facts with replay semantics.** Events describe completed state
   transitions and must preserve enough identity, ordering, and cursor
   information for clients to resume safely within the retained history.
9. **Isolation is a platform responsibility behind a boundary.** Process,
   cgroup, pidfd, device-filter, and GPU primitives belong to the sandbox or
   hardware adapter boundary. The Kernel depends on their declared outcomes,
   not on a specific OS or vendor implementation.

## Core vocabulary

The durable Kernel model uses the following nouns. The v1 contract defines
their concrete representations and valid transitions.

- **Principal:** authenticated actor whose authority is auditable.
- **Provider:** authoritative owner of a class of local resources.
- **Resource:** schedulable or allocatable local capability under one provider.
- **Capability:** stable, provider-normalized description of what a resource
  can do.
- **Lease:** time-bounded authority over one or more resources.
- **Fence:** monotonic authority token that rejects stale mutations.
- **Worker:** local execution instance bound to an owner and lifecycle.
- **Operation:** observable asynchronous unit of requested work.
- **Endpoint:** authorized connection description, not the payload relay itself.
- **Event:** durable fact about a semantic transition, available for bounded
  replay.

Identity is opaque and stable within its contract scope. A generation or fence
is never a cosmetic field: it is part of the stale-authority and recovery
story.

## Evolution discipline

The following are valuable design directions, but are **not** implicit v1
requirements: namespace-scoped identity, a snapshot-plus-stream event model,
server-streaming delivery, new provider actions, and additional ABI or JVM
projections. Each requires a proposal that states its versioning and
compatibility impact, updates the authoritative semantic contract, and adds
cross-projection conformance coverage before it becomes a promise.

Before accepting a Kernel semantic change, answer these questions:

1. Which core concept, authority rule, or state transition changes?
2. Is the change backward compatible for persisted state, clients, and
   adapters?
3. How do identity, generation, lease, and fence behave after restart or
   reconciliation?
4. Which contract and golden/TCK cases prove the behaviour in every supported
   projection?
5. Does the change preserve the Kernel's boundary rather than importing a
   product, scheduler, driver, or vendor concern?

## Related operational material

The [Kernel runtime baseline](../operations/kernel-runtime.md) describes the
current deployment and operating shape. It is intentionally separate from these
design goals: runtime instructions may evolve with platform support, whereas
the principles above change only through an explicit architectural decision.
---

<!-- Chinese Translation / 中文翻译 -->

# CYRENE Kernel 设计目标

**状态：**长期有效的设计指引；不是版本化线协议契约，也不是实现检查清单。

本文记录指导 Kernel 演进的方向和不变量。具体 v1 含义、字段、错误处理和兼容规则仅由 [Kernel Semantic Contract v1](../contracts/kernel-semantic-contract-v1.md) 定义。当前预期实现或验证的工作记录在 [Kernel Execution Goals](../operations/kernel-execution-goals.md)。

## 目的与边界

CYRENE Kernel 是节点资源及其安全使用方式的本地 authority。它拥有 identity、资源观测、Lease authority、fencing、本地 Worker 生命周期、本地 control-plane 访问，以及这些状态变更所产生的事实。它不是全局 scheduler、vendor SDK、device driver 或 Product/business 层。

架构仓库边界由 [Platform clean boundary](../governance/platform-clean-boundary.md) 规定。旧的 [Engine Architecture Blueprint](../CYRENE_ENGINE_ARCHITECTURE_BLUEPRINT.md) 仅作为历史背景保留。

Platform 和资源专属隔离仍由外部 adapter 或 Service 负责；Kernel 保持通用语义 authority。

## 设计原则

1. **先定义语义模型，再做投影。**Kernel 概念只在语义模型中定义一次。gRPC、UDS、C ABI、JVM、Kotlin 或其他传输和语言视图都是投影，而不是独立事实来源。
2. **明确本地 authority。**节点只能授权它拥有的资源和 Worker。每项资源同一时刻必须有一个权威 Provider；每个可变更状态的操作都必须可追溯到某个 Principal。
3. **Lease 携带 authority 与 fencing。**Lease 有期限、可续期，并绑定 fence。过期 authority 必须被拒绝，不能静默覆盖更新的持有者。
4. **Provider 规范化异构性。**Hardware、execution、network 等 backend 对外提供稳定的语义 capability 和 resource。厂商细节可以保留为 Provider metadata，但不能泄漏到 Kernel 的通用 authority 模型。
5. **通过 reconciliation 修复观测状态。**Inventory 报告 Provider 看到的事实；reconciliation 决定它们是否匹配期望状态，并生成必要修复工作。两者不能混为一谈。
6. **Worker 是受控的本地进程。**Worker 有 owner、generation、可观测生命周期和故障路径。Kernel recovery 可以清理或协调已知工作，但不得接管未经验证的外部进程。
7. **控制路径与数据路径分离。**Kernel 负责授权并描述 endpoint，不是高吞吐 payload 转发路径。暴露 endpoint 时必须保留 ownership 和 authorization 信息。
8. **Event 是具有 replay 语义的事实。**Event 描述已完成的状态转换，并必须保留足够的 identity、顺序和 cursor 信息，使客户端能在保留历史范围内安全恢复。
9. **隔离是 Platform 通过边界提供的责任。**进程、cgroup、pidfd、device filter 和 GPU 基元属于 sandbox 或 hardware adapter 边界。Kernel 依赖它们声明的结果，而不依赖特定 OS 或厂商实现。

## 核心词汇

持久 Kernel 模型使用以下名词。v1 契约定义其具体表示形式和有效状态转换。

- **Principal：**经过认证且 authority 可审计的 actor。
- **Provider：**一类本地资源的权威 owner。
- **Resource：**某个 Provider 下可调度或分配的本地 capability。
- **Capability：**对 Resource 能力的稳定、经 Provider 规范化的描述。
- **Lease：**对一个或多个 Resource 的限时 authority。
- **Fence：**拒绝过期变更的单调 authority token。
- **Worker：**绑定 owner 和生命周期的本地执行实例。
- **Operation：**可观测的异步工作单元。
- **Endpoint：**经过授权的连接描述，不是 payload relay 本身。
- **Event：**关于语义状态转换的持久事实，支持有界 replay。

Identity 在所属契约范围内是不透明且稳定的。Generation 或 fence 不是装饰字段，而是过期 authority 和 recovery 机制的一部分。

## 演进纪律

以下内容是有价值的设计方向，但**不是** v1 隐含要求：namespace-scoped identity、snapshot-plus-stream event 模型、server-streaming delivery、新 Provider action，以及额外 ABI/JVM projection。每项变更都必须有提案，说明版本与兼容性影响，更新规范语义契约，并在承诺实现前增加跨投影一致性覆盖。

批准 Kernel 语义变更前，请回答：

1. 哪个核心概念、authority 规则或状态转换发生变化？
2. 对持久状态、客户端和 adapter 是否向后兼容？
3. 重启或 reconciliation 后 identity、generation、lease 和 fence 如何处理？
4. 哪些 contract 与 golden/TCK case 能证明所有受支持 projection 的行为？
5. 变更是否保留 Kernel 边界，且不引入 Product、scheduler、driver 或厂商关注点？

## 相关运维材料

[Kernel runtime baseline](../operations/kernel-runtime.md) 描述当前部署与运行形态。它与这些设计目标分开维护：runtime 指南会随平台支持情况演进，而上述原则只有通过明确的架构决策才能改变。
