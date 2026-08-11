# Kernel Semantic TCK v1

This directory is the executable compatibility baseline for the frozen
`cyrene.kernel.semantic/v1` contract. The normative specification remains
`docs/contracts/kernel-semantic-contract-v1.md`; these TSV fixtures are its
machine-readable acceptance vectors. Python, Kotlin and Rust independently
evaluate the same decisions.

The suite freezes four areas that must not drift between projections:

- identifier grammar, timestamp range and numeric limits;
- complete Lease, Worker and Operation transition matrices;
- semantic revision negotiation and exact Capability/Quantity matching;
- Lease and EndpointGrant fencing/expiry authority.
- Event source, replay-gap and bounded page behavior.

Run the dependency-free reference runners from this directory:

```text
python python/kernel_semantic_tck.py
kotlinc kotlin/KernelSemanticTck.kt -include-runtime -d kernel-semantic-tck.jar
java -jar kernel-semantic-tck.jar .
```

The Rust projection consumes these files from its unit tests. A language SDK
conforms only when its public types and transport decoders pass these same
vectors, reject missing/unknown required values, and do not add weaker local
defaults. Passing this suite does not certify a transport, authentication
policy, scheduler or Provider implementation.

Frozen-v1 change rule: existing accepted inputs, state transitions, authority
decisions, field meanings and numeric limits cannot change in place. A change
to any of them requires semantic v2. New optional projection fields are only
allowed when ignoring them preserves every v1 decision.

## Coverage matrix

The suite now exercises every v1 action and every v1 `reason_code` from
`docs/contracts/kernel-semantic-contract-v1.md` §11.

### Actions (17/17)

| Action | Fixture file |
|---|---|
| `NEGOTIATE` | `negotiation.tsv` |
| `REGISTER_PROVIDER` | `provider.tsv` (new) |
| `PUBLISH_INVENTORY` | `provider.tsv` (new) |
| `RECONCILE_PROVIDER` | `provider.tsv` (new) |
| `ACQUIRE_LEASE` | `lease_acquire.tsv` (new) |
| `RENEW_LEASE` | `renewal.tsv` |
| `RELEASE_LEASE` | `transitions.tsv`, `authority.tsv` |
| `START_WORKER` | `transitions.tsv` (worker matrix) |
| `HEARTBEAT_WORKER` | worker-control TCK `semantic_scenarios.tsv` |
| `STOP_WORKER` | `transitions.tsv` (worker matrix) |
| `CREATE_OPERATION` | `transitions.tsv` (operation matrix) |
| `REPORT_OPERATION` | `transitions.tsv` (operation matrix) |
| `CANCEL_OPERATION` | `transitions.tsv` (operation matrix) |
| `PUBLISH_ENDPOINT` | `endpoint.tsv` (new) |
| `AUTHORIZE_ENDPOINT` | `authority.tsv` (grant rows) |
| `REVOKE_ENDPOINT` | `endpoint.tsv` (new) |
| `SUBSCRIBE_EVENTS` | `replay.tsv` |

### Reason codes (all covered)

| Category | Reason code | Fixture file |
|---|---|---|
| compatibility | `CONTRACT_INCOMPATIBLE` | `negotiation.tsv` |
| authentication | `AUTHENTICATION_REQUIRED` | `denials.tsv` (new) |
| authority | `AUTHORITY_DENIED` | `endpoint.tsv` (new) |
| validity | `REQUIRED_FIELD_MISSING` | `denials.tsv` (new) |
| validity | `UNKNOWN_ENUM_VALUE` | `denials.tsv` (new) |
| validity | `TEXT_INVALID` | `identifiers.tsv` |
| validity | `NAMESPACED_ID_INVALID` | `identifiers.tsv` |
| validity | `REASON_CODE_INVALID` | `denials.tsv` (new) |
| validity | `TIMESTAMP_INVALID` | `identifiers.tsv` |
| authority | `GENERATION_INVALID` | `provider.tsv` (new) |
| authority | `STALE_GENERATION` | `provider.tsv` (new) |
| authority | `FENCE_TOKEN_INVALID` | `denials.tsv` (new) |
| authority | `FENCE_MISMATCH` | `renewal.tsv` |
| authority | `LEASE_EXPIRED` | `renewal.tsv`, `lease_acquire.tsv` (new) |
| authority | `LEASE_NOT_ACTIVE` | `renewal.tsv` |
| authority | `LEASE_RENEWAL_INVALID` | `renewal.tsv` |
| lifecycle | `STATE_TRANSITION_INVALID` | `transitions_invalid.tsv` (new) |
| lifecycle | `REPLAY_GAP` | `replay.tsv` (`GAP`) |
| lifecycle | `EVENT_SOURCE_CHANGED` | `replay.tsv` (`SOURCE_CHANGED`) |
