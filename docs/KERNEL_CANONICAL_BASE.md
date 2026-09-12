# Kernel canonical base

Yield Stage 3 and later training work must target the current Platform Kernel
integration snapshot:

- branch: origin/develop
- commit: bc3327bdbe1edba6449a0f34d6954b022e4c3dd0

origin/main is an older snapshot. Do not restore cy.llm.AgentService or
ExecuteCommandStream as the training execution contract.

This file is documentation only. It does not change the GitHub default branch
or replace the release branch/tag policy.
