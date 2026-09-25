# End-to-End Flow: Cancellation, Retry, and Lost Handling

This document details how the Control Plane handles lease timeouts, worker crashes, and attempt fencing.

---

## Failure Recovery & Fencing Model

```text
┌─────────────────────────────────────────────────────────────┐
│                     RECONCILER TIMELINE                     │
├─────────────────────────────────────────────────────────────┤
│  Attempt 1 (Generation 1) starts with Lease TTL = 300s      │
│  ... Worker becomes unresponsive (heartbeat missed) ...     │
│  Lease expires ──► Kernel fences Attempt 1                  │
│  Reconciler marks Attempt 1 as LOST                         │
│  Reconciler spawns Attempt 2 with Generation 2              │
│  If Attempt 1 wakes up late: Kernel rejects stale I/O       │
│  because Lease Token & Generation 1 are invalid!            │
└─────────────────────────────────────────────────────────────┘
```

1. **Monotonic Generation**: Every retry increments the `Generation` counter. Late responses from zombie workers are discarded.
2. **Lease Expiry Fencing**: If a worker loses its lease, the Kernel revokes device access and terminates the process tree.
---

<!-- Chinese Translation / 中文翻译 -->

# 端到端流程：取消、重试与 LOST 处理

本文说明 Control Plane 如何处理 Lease 超时、Worker 崩溃和 attempt fencing。

---

## 故障恢复与 fencing 模型

```text
┌─────────────────────────────────────────────────────────────┐
│                     RECONCILER TIMELINE                     │
├─────────────────────────────────────────────────────────────┤
│  Attempt 1 (Generation 1) starts with Lease TTL = 300s      │
│  ... Worker becomes unresponsive (heartbeat missed) ...     │
│  Lease expires ──► Kernel fences Attempt 1                  │
│  Reconciler marks Attempt 1 as LOST                         │
│  Reconciler spawns Attempt 2 with Generation 2              │
│  If Attempt 1 wakes up late: Kernel rejects stale I/O       │
│  because Lease Token & Generation 1 are invalid!            │
└─────────────────────────────────────────────────────────────┘
```

1. **generation 单调递增**：每次重试都会增加 `Generation` 计数器。来自僵尸 worker 的延迟响应会被丢弃。
2. **Lease 过期 fencing**：worker 丢失 Lease 时，Kernel 会撤销设备访问权限并终止整个进程树。
