# Operations | 运维

This directory explains runtime operation, node-agent behavior, execution
goals, and verified installation evidence.

本目录说明运行时操作、Node Agent 行为、执行目标和已验证安装证据。

| File | Responsibility | 文件职责 |
| --- | --- | --- |
| `kernel-runtime.md` | Kernel runtime operation. | Kernel 运行时操作 |
| `../architecture/system-adapter.md` | System Adapter deployment boundary. | System Adapter 部署边界 |
| `../architecture/sandbox-adapter.md` | Sandbox Adapter backend boundary. | Sandbox Adapter 后端边界 |
| `node-agent.md` | Node Agent operation and ownership. | Node Agent 操作与归属 |
| `kernel-execution-goals.md` | Execution safety goals. | 执行安全目标 |
| `verified-installation-record.md` | Installation evidence record. | 安装证据记录 |

## Suggested reading | 推荐顺序

Read `kernel-runtime.md`, then `node-agent.md`, and use the installation record
when investigating deployment provenance.

先读 `kernel-runtime.md`，再读 `node-agent.md`；排查部署来源时参考安装记录。

System 与 Sandbox Adapter 的中文镜像见 [`../zh-CN/operations/`](../zh-CN/operations/README.md)
以及 [`../zh-CN/architecture/`](../zh-CN/architecture/README.md)。
---

<!-- Chinese Translation / 中文翻译 -->

# 运维

本目录说明 runtime 操作、Node Agent 行为、执行目标和已验证安装证据。

| 文件 | 职责 |
|---|---|
| `kernel-runtime.md` | Kernel runtime 操作。 |
| `../architecture/system-adapter.md` | System Adapter 部署边界。 |
| `../architecture/sandbox-adapter.md` | Sandbox Adapter 后端边界。 |
| `node-agent.md` | Node Agent 操作与归属。 |
| `kernel-execution-goals.md` | 执行安全目标。 |
| `verified-installation-record.md` | 安装证据记录。 |

## 推荐顺序

先读 `kernel-runtime.md`，再读 `node-agent.md`；排查部署来源时使用安装记录。

System 与 Sandbox Adapter 的中文镜像见 [`../zh-CN/operations/`](../zh-CN/operations/README.md) 和 [`../zh-CN/architecture/`](../zh-CN/architecture/README.md)。
