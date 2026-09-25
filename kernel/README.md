# Kernel

The kernel is the small, pure-safe Rust decision base. It owns lifecycle policy
and authorization, not Linux privilege or AI business decisions.

Allowed responsibilities:

- lease/fence validation, launch/stop decisions, instance state, and watchdog policy;
- generic UDS clients for separately supervised sandbox and hardware adapters;
- protocol validation and local IPC primitives;
- resource assignment enforcement supplied by the framework;
- structured lifecycle and failure events.

Not allowed here: direct process spawning, cgroup/namespace/device operations,
pidfd/BPF/prctl, hardware discovery, model selection, dataset processing,
training policy, inference engines, gateway business rules, user interfaces,
or evaluation logic.
---

<!-- Chinese Translation / 中文翻译 -->

# Kernel

Kernel 是精简、纯安全的 Rust 决策基础。它拥有生命周期策略和授权，不拥有 Linux 特权或 AI 业务决策。

允许的职责：

- Lease/fence 校验、启动/停止决策、instance 状态和 watchdog 策略；
- 连接到独立监管的沙箱与硬件 adapter 的通用 UDS client；
- 协议校验与本地 IPC 基元；
- 执行 Framework 提供的资源分配强制策略；
- 结构化生命周期事件与故障事件。

此处禁止：直接启动进程、cgroup/namespace/device 操作、pidfd/BPF/prctl、硬件发现、模型选择、数据集处理、训练策略、推理引擎、网关业务规则、用户界面或评估逻辑。
