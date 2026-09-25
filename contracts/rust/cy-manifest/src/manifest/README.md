# cy-manifest projections | cy-manifest 投影

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `artifact.rs` | Provider-neutral Artifact identity and portable directory index. | 与供应商无关的 Artifact 身份和 portable directory 索引。 |
| `plugin.rs` | Generic Plugin discovery, compatibility, launch, and lock records. | 通用 Plugin 发现、兼容、启动与锁定记录。 |
| `mod.rs` | Public module exports. | 公开模块导出。 |

Product payload and lifecycle models are intentionally absent.
Product 载荷与生命周期模型不在此目录中。
---

<!-- Chinese Translation / 中文翻译 -->

# cy-manifest 投影

| 文件 | 职责 |
|---|---|
| `artifact.rs` | 与 Provider 无关的 Artifact identity 和 portable directory index。 |
| `plugin.rs` | 通用 Plugin discovery、兼容性、启动和 lock record。 |
| `mod.rs` | 公开 module 导出。 |

这里有意不包含 Product payload 和生命周期模型。
