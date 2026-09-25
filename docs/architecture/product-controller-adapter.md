# Architecture Deep-Dive: Product, Controller & Adapter

This document explains why high-level Product state is strictly separated from low-level execution primitives.

---

## Why `TrainingRun` Is Not a Kernel `Operation`

| Concept | Layer | Lifecycle & Scope | Failure Recovery |
|---|---|---|---|
| **`TrainingRun`** | Product (Yield) | Spans hours/days, contains multiple phases (setup, fine-tune, eval, export) | Re-planned by Reconciler upon step failure |
| **`Operation`** | Kernel (Platform) | A single discrete, time-bounded execution attempt inside a sandbox | Terminated on lease expiry; produces exit code |

A `TrainingRun` is a business entity with user-visible history, billing records, and domain metrics. A Kernel `Operation` is an ephemeral execution sandbox. Conflating them would tie the Kernel to domain-specific persistence schemas.
---

<!-- Chinese Translation / 中文翻译 -->

# 架构深入解析：Product、Controller 与 Adapter

本文说明为什么高层 Product 状态必须与底层执行基元严格分离。

---

## 为什么 `TrainingRun` 不是 Kernel 的 `Operation`？

| 概念 | 层 | 生命周期与范围 | 故障恢复 |
|---|---|---|---|
| **`TrainingRun`** | Product（Yield） | 持续数小时或数天，包含多个阶段（准备、微调、评估、导出） | 某步骤失败时由 Reconciler 重新规划 |
| **`Operation`** | Kernel（Platform） | 沙箱内单次离散、限时的执行尝试 | Lease 到期时终止；生成退出码 |

`TrainingRun` 是具有用户可见历史、计费记录和领域指标的业务实体。Kernel `Operation` 是临时执行沙箱。将二者混为一谈会使 Kernel 与特定领域的持久化 schema 耦合。
