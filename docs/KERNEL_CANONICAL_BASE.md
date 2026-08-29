# Kernel canonical base

Yield Stage 3 and later training work must target this Kernel snapshot:

- branch: origin/develop
- commit: 27e69c469f782df9fa66051b46c204fce1165fc7

origin/main is an older snapshot. Do not restore cy.llm.AgentService or
ExecuteCommandStream as the training execution contract.

This file is documentation only. It does not change the GitHub default branch.
