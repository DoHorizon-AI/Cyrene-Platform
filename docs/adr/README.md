# Architecture Decision Records | 架构决策记录

ADRs capture decisions that constrain repository ownership, runtime authority,
plugin boundaries, adapters, and release governance.

ADR 记录仓库归属、运行时权威、插件边界、适配器和发布治理方面的约束性决策。

| File group | Responsibility | 文件组职责 |
| --- | --- | --- |
| `ADR-00*.md` | Core ownership and execution decisions. | Core 归属与执行决策 |
| `ADR-PLUGIN-*.md` | Plugin execution and runtime boundaries. | 插件执行与运行时边界 |
| `ADR-*-BOUNDARY.md` | Hardware, sandbox, and repository boundaries. | 硬件、沙箱与仓库边界 |

## Suggested reading | 推荐顺序

Start with `ADR-001`, `ADR-002`, `ADR-003`, and `ADR-004`; read the boundary
records before changing a cross-process interface.

先读 `ADR-001` 至 `ADR-004`；修改跨进程接口前阅读对应边界决策。
---

<!-- Chinese Translation / 中文翻译 -->

# 架构决策记录

ADR 记录约束仓库归属、运行时 authority、plugin 边界、adapter 和发布治理的决策。

| 文件组 | 职责 |
|---|---|
| `ADR-00*.md` | Core 归属与执行决策。 |
| `ADR-PLUGIN-*.md` | Plugin 执行与 runtime 边界。 |
| `ADR-*-BOUNDARY.md` | 硬件、沙箱和仓库边界。 |

## 推荐顺序

先读 `ADR-001` 至 `ADR-004`；修改跨进程 interface 前，先阅读对应边界决策。
