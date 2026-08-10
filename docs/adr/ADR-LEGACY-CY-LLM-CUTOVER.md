# ADR-LEGACY-CY-LLM-CUTOVER: Internal Legacy Contract Cutover

- Status: Implemented / Historical
- Date: 2026-08-10
- Scope: former version-0 Proto files, generated Rust binding, and Node Agent
  service scaffold

## Decision

The former version-0 package was an internal migration artifact, not a public
compatibility contract. P0 froze it: no new RPC, message, field, command,
consumer, or business behavior could be added.

P1 replaced the public Rust surface with `cyrene::core::v1`, migrated Node
Agent state to the transport-free `NodeControlSession`, and removed the old
Proto files, generated binding, and transition allowlist. Any reintroduction
of the former names in runtime or contract source is now a governance
failure.

The separate `cy.plugin.v1` stdio protocol was intentionally left outside this
cutover.

## Non-goals

This historical ADR does not define Core v1 service behavior, network
transport, mTLS/UDS, lease admission, sandboxing, or plugin startup. Those are
P2/P3 work.

## Deletion gate

The P1 deletion gate was satisfied when:

1. Core v1 contracts and generated bindings build from a clean checkout;
2. cy-node-agent uses the replacement contract and has no remote command echo;
3. no runtime or contract source path contains the former legacy references;
4. Rust CI and the replacement fixture tests pass;
5. deletion is one local reviewed cutover commit after the Core v1 baseline.
