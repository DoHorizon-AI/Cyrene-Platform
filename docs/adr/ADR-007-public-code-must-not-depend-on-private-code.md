# ADR-007: Public Code Must Not Depend on Private Code

- **Status**: ACCEPTED
- **Date**: 2026-08-26

## Context
To support both open-source community distribution and future enterprise/commercial offerings, a strict dependency direction is required.

## Decision
Dependency direction is strictly: $\text{PRIVATE} \longrightarrow \text{PUBLIC}$. Public code (`Cyrene-Platform`, `Cyrene-Plugins`) must never import, link, or conditionally reference private commercial or enterprise packages.

## Why
Public foundation code must remain 100% buildable, testable, and functional in Community mode without requiring private repositories or licensing keys.

## Alternatives Considered
- *Conditional `try/except ImportError` Hooks in Core*: Rejected because it pollutes public source with proprietary references and obscures open-source boundaries.

## Consequences
- Private and enterprise capabilities are injected dynamically at runtime via public Capability interfaces.
