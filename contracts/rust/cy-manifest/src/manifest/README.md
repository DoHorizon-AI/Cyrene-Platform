# cy-manifest projections | cy-manifest 投影

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `artifact.rs` | Provider-neutral Artifact identity and portable directory index. | 与供应商无关的 Artifact 身份和 portable directory 索引。 |
| `plugin.rs` | Generic Plugin discovery, compatibility, launch, and lock records. | 通用 Plugin 发现、兼容、启动与锁定记录。 |
| `mod.rs` | Public module exports. | 公开模块导出。 |

Product payload and lifecycle models are intentionally absent.
Product 载荷与生命周期模型不在此目录中。
