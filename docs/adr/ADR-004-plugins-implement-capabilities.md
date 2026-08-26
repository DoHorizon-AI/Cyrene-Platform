# ADR-004: Plugins Implement Capabilities

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
Cyrene must support diverse AI ecosystems (HuggingFace, vLLM, DeepSeek, FAISS, SQLite, FastMCP) without bloating core repositories.

## Decision
All modular implementations are packaged as Plugins adhering to versioned Capability contracts (e.g. `model.analyzer.v1`, `storage.provider.v1`). Plugins are discovered dynamically via official or workspace catalogs.

## Why
Decouples framework evolution from concrete third-party library dependencies. Upgrading HuggingFace transformers does not require rebuilding or redeploying Cyrene-Platform.

## Alternatives Considered
- *Static In-Tree Drivers*: Hardcode all model and storage drivers in Cyrene-Platform. Rejected due to dependency bloat and diamond version conflicts.

## Consequences
- Plugins must provide manifest metadata and conform to capability schemas validated by the conformance TCK.
