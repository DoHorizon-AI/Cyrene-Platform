# CYRENE Platform Documentation

This directory is the bilingual learning map for the CYRENE Platform core.
It explains the repository boundaries, contracts, runtime layers, and the
recommended path from protocol definitions to executable adapters.

本目录是 CYRENE Platform 核心仓库的双语学习地图，说明仓库边界、协议契约、
运行时分层，以及从协议定义到可执行适配器的推荐阅读路径。

## Start here | 从这里开始

1. [`start-here/00-what-is-cyrene.md`](start-here/00-what-is-cyrene.md) — platform purpose and vocabulary.
2. [`start-here/02-repository-map.md`](start-here/02-repository-map.md) — repository ownership and boundaries.
3. [`architecture/overview.md`](architecture/overview.md) — end-to-end component map and data flow.
4. [`architecture/system-adapter.md`](architecture/system-adapter.md) — system facts and target-specific host adapter.
5. [`architecture/sandbox-adapter.md`](architecture/sandbox-adapter.md) — sandbox port, native backend, and Docker boundary.
6. [`contracts/kernel-semantic-contract-v1.md`](contracts/kernel-semantic-contract-v1.md) — normative semantic authority.
7. [`flows/plugin-resolution-and-invocation.md`](flows/plugin-resolution-and-invocation.md) — a concrete execution path.

## Documentation map | 文档地图

| Directory | Responsibility | 目录职责 |
| --- | --- | --- |
| [`adr/`](adr/README.md) | Architecture decisions and non-negotiable boundaries. | 架构决策与不可逾越的边界 |
| [`api/`](api/README.md) | Capability and public API index. | 能力与公开 API 索引 |
| [`architecture/`](architecture/README.md) | Layering, authority, and data-flow explanations. | 分层、权威归属与数据流说明 |
| [`contracts/`](contracts/README.md) | Normative semantic and wire contracts. | 规范语义契约与线协议 |
| [`development/`](development/README.md) | Branching, dependencies, and local development guidance. | 分支、依赖与本地开发指南 |
| [`flows/`](flows/README.md) | Sequence-oriented runtime walkthroughs. | 按时序说明运行时流程 |
| [`governance/`](governance/README.md) | Ownership, CI trust, repository policy, and API naming constitution. | 责任归属、CI 信任、仓库治理与 API 命名宪法 |
| [`operations/`](operations/README.md) | Runtime, node-agent, and recovery operations. | 运行时、节点代理与恢复运维 |
| [`release/`](release/README.md) | Artifact, versioning, and release topology. | 产物、版本与发布拓扑 |
| [`security/`](security/README.md) | Trust, privilege, and sandbox non-guarantees. | 信任、特权与沙箱非保证 |
| [`start-here/`](start-here/README.md) | Guided onboarding and code-reading order. | 入门与代码阅读顺序 |

## Focused references | 重点参考

- [`architecture/overview.md`](architecture/overview.md) — Mermaid component map and request flow.
- [`architecture/tool-system.md`](architecture/tool-system.md) — framework extension and worker boundaries.
- [`architecture/mcp-integration.md`](architecture/mcp-integration.md) — protocol integration rules.
- [`glossary.md`](glossary.md) — English/Chinese terminology.
- [`faq.md`](faq.md) — common questions and troubleshooting.

The repository also contains historical reports and migration notes. They are
useful context, but the normative contract and current ADRs take precedence.

仓库中还保留检查报告与迁移记录，可用于理解历史背景；规范契约与当前 ADR
优先级更高。

## Language policy | 文档语言策略

English is the canonical text for normative contracts, ADRs, and release gates.
The maintained Chinese mirror for the core onboarding, architecture, operations,
and security path is under [`zh-CN/`](zh-CN/README.md). A Chinese page links
back to its English source and must preserve the same status labels and
non-guarantees; it is not a second semantic authority.

规范契约、ADR 与发布门禁以英文为 canonical source。核心入门、架构、运维和
安全路径的中文镜像位于 [`zh-CN/`](zh-CN/README.md)。每个中文页面都会链接
回英文源文档，并保持相同的状态标签与非保证声明；中文文档不是第二套语义权威。
