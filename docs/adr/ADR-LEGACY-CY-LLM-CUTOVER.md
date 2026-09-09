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

The separate `cy.plugin.v1` stdio protocol was outside the original cutover and
was removed later when Plugins-owned direct endpoints replaced it.

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

## Legacy-surface Closure (2026-09)

The repository-wide legacy-surface closure extended the above cutover to every
Cyrene repo. Scope and rules (authority baseline):

- **One concept = one authority.** Platform (execution/device/lease/Worker/Artifact
  Plane), Catalyst (Dataset/DatasetVersion/Preparation/feedback), Yield
  (TrainingRun/TrainingResult/ModelVersion — **single canonical ModelVersion
  authority**), Reactor (Deployment/Endpoint/serving), Exchange
  (Route/API protocol/auth/quota), Navigator (Conversation/AgentRun/UX/session —
  no second-message authority), Echo (Evaluation/HumanAnnotation/FeedbackSet).
  Kernel is transport-neutral.
- **Classification before deletion.** Every legacy object was classified
  (DEAD / SUPERSEDED / VALUABLE_MISSING / ACTIVE_COMPATIBILITY / SUPPORTED_MIGRATION
  / TEST_HISTORICAL_DEPENDENCY / REFERENCE_ONLY / GENERATED_LEGACY_SCHEMA /
  AMBIGUOUS_AUTHORITY) prior to any destructive action.
- **VALUABLE_MISSING → reimplemented fresh** under the canonical tree; legacy was
  never copied/imported (e.g. Echo `AnalyticsPanel` session-analytics, Exchange
  `0001_gateway_pro.sql` auth/quota schema).
- **Phase 0 preservation closure** ran first: all 9 `pre-canonical-sync` stashes
  archived and dropped; `.worktrees/` clutter archived (sanitized) and removed;
  `UNRECOVERABLE_LOCAL_WORK = 0` before any deletion.

### Per-repo outcome (PRs)
| Repo | Action | PR |
|---|---|---|
| Cyrene-Echo | delete `legacy/navigator-feedback/*` | #6 (+issue #7 reimplement) |
| Cyrene-Reactor | delete legacy stub + dead `tests/legacy/*`; move `test_checkpoint_digest.py` | #11 |
| Cyrene-Exchange | delete `legacy/*`; decouple `service.json` | #12 (+issue #13 reimplement) |
| Cyrene-Navigator | delete `legacy-dh/` (183 files) | #5 |
| Cyrene-Catalyst | move 30MB corpus → DH-LLMs; keep minimal sample; decouple `service.json` | #6 |
| Cyrene-Platform | delete `tooling/migration/` museum (38,702 LOC) | #36 |
| Cyrene-Plugins-Official | delete `archive/` stubs; **preserve** `compatibility/` (decision D) | #10 |
| Cyrene-Yield | keep `test_kernel_legacy_guard.py`; flagged `archive/icy-lunar-llamafactory` (remote) | — |
| Astrbot-Rev | no dead legacy (`CompatibilityBridge` is live) | — |
| DH-System-Internal | no legacy surface (decision B not triggered) | — |

## Legacy-reintroduction guard

To keep `archive/legacy/backup = 0` and prevent silent resurrection:

1. **Path-level CI check** fails if any of these patterns reappear in committed
   source: `^legacy/`, `^archive/`, `test_legacy_`, `plugin.legacy.toml`,
   `LEGACY_PLUGIN.md`, `*.legacy.toml`, `legacy-requirements/` (outside the two
   enterprise Dockerfiles that still consume it). Reference implementation:
   `tooling/ci/check-no-legacy-surface.sh` (wire into `architecture-governance.yml`
   once all repos stabilize).
2. **`compatibility/` is an explicit, governed adapter layer** — NOT a loophole.
   The conformance validator already rejects generic-implementation paths inside
   `compatibility/` and any import of `astrbot`/Yield/Reactor/Exchange/Navigator/
   Catalyst/Echo from generic code. Keep this guard; do not relax it while
   `compatibility/` shims remain.
3. **Authority registry** (`plugin-boundaries.json`) is the single source of truth
   for capability ownership. A new implementation must not re-introduce a second
   authority for an already-owned concept.
4. **VALUABLE_MISSING reimplementation** must land as a fresh canonical module with
   tests; never as a resurrection of the deleted legacy path.
