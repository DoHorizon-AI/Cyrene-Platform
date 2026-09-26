# FAQ and Troubleshooting | 常见问题与排障

## Why is product logic not in the Kernel?

The Kernel must remain reusable and locally authoritative. Product workflows,
model execution, training policy, and UI state belong in Framework or product
repositories.

Kernel 必须保持可复用并只拥有本地权威。产品流程、模型执行、训练策略和 UI 状态
应位于 Framework 或产品仓库。

## Where should a new capability be added?

Start with the semantic contract and capability index. Then add the narrowest
Framework extension, an out-of-process Worker if needed, and a TCK vector for a
new boundary case.

先从语义契约和能力索引开始，再增加最小的 Framework 扩展；需要时增加进程外
Worker，并为新的边界情形补充 TCK 向量。

## Why does a hardware adapter failure not crash the Kernel?

Hardware facts and vendor interactions are isolated in an Adapter Host. A lost
adapter changes admission/readiness evidence and can produce a degraded state;
it is not a reason to load vendor code into the Kernel or terminate it.

硬件事实与厂商交互隔离在适配器主机中。适配器失联会改变准入/就绪证据并进入降级
状态，但不应把厂商代码加载到 Kernel，也不应因此终止 Kernel。

## A local TCK is skipped. Is that a pass?

No. A skipped integration or database-backed test is incomplete evidence. Record
pass, skip, and block separately, and use the real fixture when acceptance needs
the external boundary.

不是。跳过集成或数据库测试不能算通过。应分别记录通过、跳过和阻塞；需要验收
外部边界时使用真实 fixture。

## How do I investigate an unexpected denial?

Trace the request through identity, revision, authority, lease, and resource
validation in that order. Compare the transport projection with the semantic
contract before changing implementation code.

按身份、版本、权威、租约、资源校验的顺序追踪请求。修改实现前，先将传输投影与
语义契约对照。

## Which branch should receive a change?

Follow the repository governance and branch model documents. Keep the task on a
dedicated branch and do not push a protected default branch directly.

遵循仓库治理与分支模型文档。改动应在独立任务分支上完成，不要直接推送受保护的
默认分支。
---

<!-- Chinese Translation / 中文翻译 -->

# 常见问题与排障

## 为什么 Kernel 中没有 Product 逻辑？

Kernel 必须保持可复用，并维持本地权威。Product 工作流、模型执行、训练策略和 UI 状态属于 Framework 或各 Product 仓库。

## 新 capability 应该加在哪里？

先从语义契约和 capability 索引开始，再添加最精简的 Framework 扩展；必要时添加进程外 Worker，并为新边界场景补充 TCK 向量。

## 为什么硬件适配器故障不会导致 Kernel 崩溃？

硬件事实和厂商交互隔离在 Adapter Host 中。适配器失联会改变准入/就绪证据，也可能使系统进入降级状态；这不构成把厂商代码加载进 Kernel 或终止 Kernel 的理由。

## 本地 TCK 被跳过，算通过吗？

不算。跳过集成测试或数据库测试代表证据不完整。应分别记录 pass、skip 和 block；验收需要外部边界时，应使用真实 fixture。

## 如何调查意外拒绝？

按身份、revision、authority、lease 和资源校验的顺序追踪请求。修改实现代码前，先对照传输投影与语义契约。

## 哪个分支应该接收变更？

遵循仓库治理与分支模型文档。将任务保留在专用分支上，不要直接推送到受保护的默认分支。
