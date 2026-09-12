# CYRENE Platform 中文文档

这是 Platform 核心文档的中文维护镜像，覆盖入门、架构、适配器、运维和安全主路径。
英文文档仍是规范契约、ADR、发布门禁和 API 语义的 canonical source；中文页面不能
引入与英文源不同的语义。

## 推荐阅读顺序

1. [系统是什么](start-here/00-what-is-cyrene.md)
2. [系统地图](start-here/01-system-map.md)
3. [仓库地图](start-here/02-repository-map.md)
4. [架构总览](architecture/overview.md)
5. [System Adapter](architecture/system-adapter.md)
6. [Sandbox Adapter](architecture/sandbox-adapter.md)
7. [Kernel 运行与部署边界](operations/kernel-runtime.md)
8. [信任与安全模型](security/threat-model.md)

## 中文文档规则

- 每个页面顶部或结尾链接英文 canonical source。
- `COMPLETE`、`PARTIAL`、`NOT_COMPLETE`、`DEFERRED` 等状态必须与英文源一致。
- 历史审计报告、迁移日志和旧版本记录不做机械镜像；需要时直接阅读原文。
- 修改规范英文文档时，必须同步检查对应中文页面是否仍然准确。

英文总索引：[docs/README.md](../README.md)。
