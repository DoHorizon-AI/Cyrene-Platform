# Platform CI Gates

Cyrene-Platform uses Azure DevOps as its hosted CI authority. A source change is
accepted only after the Platform pipeline verifies documentation, architecture
governance, JVM and .NET contract TCKs, Python SDKs and tooling, and the full
Rust workspace for the exact commit.

GitHub Actions may provide duplicate feedback, but quota or runner failures
with no executed source steps are external CI limitations and are not source
PASS evidence. Required gates must not be weakened or skipped to compensate.

Status checks and branch policies for other repositories are owned by those
repositories and by the live integration metadata in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
---

<!-- Chinese Translation / 中文翻译 -->

# Platform CI Gate

Cyrene-Platform 使用 Azure DevOps 作为托管 CI 权威。只有 Platform pipeline 针对精确提交验证文档、架构治理、JVM 和 .NET 契约 TCK、Python SDK 与工具，以及完整 Rust workspace 后，源码变更才算验收。

GitHub Actions 可以提供重复反馈，但若没有执行源码步骤便因配额或 runner 故障失败，这属于外部 CI 限制，不构成源码 PASS 证据。不得通过削弱或跳过必需 gate 来弥补这些问题。

其他仓库的状态检查和分支策略由那些仓库及 [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) 中的实时集成元数据负责。
