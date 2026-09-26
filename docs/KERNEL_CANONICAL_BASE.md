# Kernel canonical base

Yield Stage 3 and later training work must target the current Platform Kernel
integration snapshot:

- branch: origin/develop
- commit: bc3327bdbe1edba6449a0f34d6954b022e4c3dd0

origin/main is an older snapshot. Do not restore cy.llm.AgentService or
ExecuteCommandStream as the training execution contract.

This file is documentation only. It does not change the GitHub default branch
or replace the release branch/tag policy.
---

<!-- Chinese Translation / 中文翻译 -->

# Kernel 规范基线

Yield Stage 3 及后续训练工作必须以当前 Platform Kernel 集成快照为目标：

- 分支：origin/develop
- 提交：bc3327bdbe1edba6449a0f34d6954b022e4c3dd0

origin/main 是较早的快照。不要恢复 cy.llm.AgentService 或 ExecuteCommandStream 作为训练执行契约。

本文件仅用于说明，不会更改 GitHub 默认分支，也不会替代发布分支或标签策略。
