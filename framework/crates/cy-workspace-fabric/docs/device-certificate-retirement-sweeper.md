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

<!-- device-certificate-retirement-postgres-retry -->
## PostgreSQL retry schedule addendum

Migration `device_authorization/0006_retirement_retry_schedule` adds
`retirement_attempt_count` and `retirement_next_attempt_at_unix_ms`. Existing
`RetirementPending` rows become eligible immediately at migration time because
their pre-migration failure count is unknown. The counter records persisted CA
retirement failures; `state_payload` remains the source of the latest failure
kind.

The production source reads candidate authorization IDs only. Due deliveries
and retries use bounded adapter pages. The retry scan filters by the primary
database clock and orders by retry time and ID. A failed result is persisted
with the record revision CAS and receives exponential delay from one second up
to one hour. Failed rows leave the current due page so later pending rows can
be scanned. The delivery-to-retirement CAS checks row revision, registration
binding, device identity, authorization generation, and deadline against the
database clock, then initializes retry eligibility.

This provides PostgreSQL scanning and durable retry state only. The real CA
revoker and production scheduler are still absent, so hosts must keep this
worker disabled.

Every manager path that can resume retirement now checks the exact persisted
revision and retry schedule against the PostgreSQL clock immediately before a
CA call. A false result leaves the record pending, and a storage error prevents
the call. This check is not a distributed lease: concurrent workers may both
pass it, so the configured CA revoker must be idempotent by authorization ID
and certificate fingerprint.

## PostgreSQL 重试计划补充

迁移 `device_authorization/0006_retirement_retry_schedule` 为授权记录增加
`retirement_attempt_count` 和 `retirement_next_attempt_at_unix_ms`。升级时，已有的
`RetirementPending` 记录按数据库时钟立即进入可重试队列；其迁移前失败次数未知，计数从
零开始。此计数记录已持久化的失败撤销调用，不替代 `state_payload` 中保存的最后失败类型。

生产 source 只读取候选授权 ID。due delivery 和重试候选都受 adapter 页大小限制；重试扫描用
数据库时钟过滤 `retirement_next_attempt_at_unix_ms`，并按重试时间和 ID 排序。失败结果通过
记录 revision CAS 持久化，重试延迟从 1 秒指数增长、最长 1 小时；失败记录离开当前 due 页，
后续 pending 记录可以继续被扫描。delivery 转入 `RetirementPending` 时，在同一 CAS 中校验
行 revision、registration binding、设备身份、授权代次及数据库截止时间，并初始化重试计划。

这只完成 PostgreSQL 扫描与持久重试状态。真实 CA revoker 和生产调度器仍未配置，host 必须继续
禁用此 worker。

所有 manager 撤销恢复路径都会在 CA 调用前，以 PostgreSQL 时钟检查精确持久化 revision 和重试计划。
未到期时保留 pending 状态；存储检查失败则不调用 CA。此检查不是分布式租约：并发 worker 仍可能同时
通过检查，因此配置的 CA revoker 必须以 authorization ID 和证书指纹实现幂等。
<!-- /device-certificate-retirement-postgres-retry -->
