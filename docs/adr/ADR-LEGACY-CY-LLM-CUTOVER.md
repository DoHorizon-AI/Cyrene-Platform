# ADR-LEGACY-CY-LLM-CUTOVER: Internal Legacy Contract Cutover

- Status: Approved / Migration Governance
- Date: 2026-08-10
- Scope: contracts/proto/ai_service.proto, contracts/proto/agent_service.proto, and their generated Rust binding

## Decision

cy.llm is an internal migration artifact, not a public compatibility contract.
The project has no external consumers that require preserving its API shape.
P0 therefore freezes it: no new RPC, message, field, command, consumer, or
business behavior may be added.

P0 keeps the existing files compiling so the current workspace remains
verifiable. P1 must introduce the replacement Core v1 contract, migrate the
remaining Rust node and binding code, and delete the legacy Proto files and
generated binding in one reviewed cutover.

The legacy allowlist under contracts/legacy is the only permitted source
reference set during the transition. Any new cy.llm or AgentService consumer is
a governance failure.

## Non-goals

This ADR does not change the cy.llm wire numbers, implement Core v1, create a
Buf generation pipeline, or change Node Agent behavior. Those changes belong to
P1 and later.

## Deletion gate

Legacy deletion is allowed only when:

1. Core v1 contracts and generated bindings build from a clean checkout;
2. cy-node-agent and all remaining Core consumers use the replacement contract;
3. no tracked source path remains on the legacy allowlist;
4. full Rust CI and the replacement contract conformance tests pass;
5. the cutover is one atomic, reviewed change with a documented rollback point.
