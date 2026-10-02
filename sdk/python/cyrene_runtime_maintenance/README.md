# Cyrene Runtime Maintenance SDK

The Python SDK talks to the installed `cyrene-runtime-maintenance` broker over a private Unix socket using one JSON request and one JSON response per connection. It provides update readiness, maintenance transitions, and durable Product task activity calls.

| File | Responsibility |
| --- | --- |
| `src/cyrene_runtime_maintenance/client.py` | Synchronous authenticated Unix-socket client for broker and Product hooks. |
| `src/cyrene_runtime_maintenance/__init__.py` | Public SDK exports. |
| `tests/test_client.py` | JSON-line request and fail-closed transport behavior. |

Product services should reconcile durable nonterminal tasks at startup, send source heartbeats every ten seconds, and call `complete_task` only after a durable terminal transition.

## Maintenance transaction IDs

`begin_maintenance(request_id, ...)` and `end_maintenance(request_id, ...)` take the same stable **maintenance transaction ID**. The broker journals this ID with the plan and uses it to recover an idempotent Begin response or End receipt after a lost reply or broker restart. Pass the original Begin transaction ID to End.

The SDK creates a separate, fresh top-level `request_id` for each JSON IPC call. That outer value is only a request/response correlation ID; it is not the maintenance transaction ID and must not be passed to `end_maintenance`.

```python
transaction_id = "workspace-update-2026-10-01-01"
begun = operator.begin_maintenance(
    transaction_id,
    expected_gate_generation=readiness["gate_generation"],
    expected_activity_sources=installed_sources,
    user_confirmed_restart=True,
    plan_id=plan_id,
    plan_digest=plan_digest,
    component_artifact_digests=artifact_digests,
)
token = begun["maintenance_token"]

# After apply or a healthy rollback, reuse transaction_id and token.
operator.end_maintenance(
    transaction_id,
    token,
    outcome="SUCCESS",
    healthy=True,
)
```

For example, the wire envelope for Begin has a generated outer ID while `params.request_id` is the stable transaction ID:

```json
{"request_id":"f9cfc8d0-cff2-4a12-9f21-f20187dbb27e","method":"BeginMaintenance","auth":{"operator_token":"<operator-token>"},"params":{"request_id":"workspace-update-2026-10-01-01","target_kind":"PACKAGE_ONLY","requires_restart":true,"expected_gate_generation":42,"expected_catalog_generation":7,"expected_activity_sources":["cyrene-yield"],"user_confirmed_restart":true,"plan_id":"plan-42","plan_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","component_artifact_digests":{"cyrene-yield":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}}
```

End uses a new outer correlation ID and the original transaction ID in `params`:

```json
{"request_id":"85b8a6fb-7308-4986-b338-cdb20787a6da","method":"EndMaintenance","auth":{"operator_token":"<operator-token>"},"params":{"request_id":"workspace-update-2026-10-01-01","maintenance_token":"<durable-token>","outcome":"SUCCESS","healthy":true,"target_kind":"PACKAGE_ONLY"}}
```

中文：Begin 与 End 的 `params.request_id` 是同一个持久维护事务 ID；每次 IPC 外层的 `request_id` 都由 SDK 新生成，只用于匹配请求和响应。崩溃恢复时重试 Begin 或 End，应复用维护事务 ID，End 还要复用原 token 与相同结果参数。
