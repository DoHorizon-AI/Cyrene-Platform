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

