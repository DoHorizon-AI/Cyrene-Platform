# Device certificate retirement sweeper

The sweeper processes two bounded advisory queues: certificate deliveries past
their acknowledgement deadline or certificate expiry, and records already in
`RetirementPending`. It addresses the case where a device received a
certificate but never returned to acknowledge it.

For a due `DeliveryPending` row, the manager first asks the durable store to
atomically compare the row revision and state and recheck the delivery deadline
or certificate `notAfter` against the authorization database clock. Only a
successful `DeliveryPending -> RetirementPending` reservation permits a call to
the CA retirement port. A failed or ambiguous CA result remains pending; a
later pass retries with the same authorization ID and certificate fingerprint.
Only CA confirmation allows the manager to write `DeliveryExpired` or another
retirement terminal state.

## Host requirements

- Construct the work source and manager against the same authorization primary
  database. Do not use a read replica for either the sampled database time or
  the advisory scans.
- Schedule `run_once` at a fixed interval. Each pass is capped at 100 unique
  authorization IDs, alternates due deliveries with pending retirements, and
  has a process-local minimum interval. Do not call it in a tight retry loop.
- The CA retirement port must be durable and idempotent by authorization ID
  and certificate fingerprint. Multi-replica hosts must account for the fact
  that the process-local interval does not impose a fleet-wide rate limit.
- Keep the worker disabled until the PostgreSQL adapter implements the sweep
  source using database time and the due-state CAS, the real CA revoker is
  configured, and the host has an operational scheduler.

The worker emits aggregate counts only. It does not log authorization IDs,
device IDs, certificate bytes, raw codes, or backend error text. Storage and
per-row failures are reported as stable errors and aggregate counts so later
passes can retry unresolved records.

## Local gate

Run the fabric crate's targeted retirement worker tests and formatting check
after changing this path:

```sh
cargo test --manifest-path framework/crates/cy-workspace-fabric/Cargo.toml device_authorization_sweeper
cargo test --manifest-path framework/crates/cy-workspace-fabric/Cargo.toml retirement_worker_entrypoint_reserves_before_revoke_and_retries_unknown_result
cargo fmt --manifest-path framework/crates/cy-workspace-fabric/Cargo.toml -- --check
```
