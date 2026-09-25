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

### Actions (18/18)

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
| `READ_EVENTS` | `replay.tsv` |
| `WATCH_EVENTS` | `replay.tsv` initial replay and continuity vectors |

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
---

<!-- Chinese Translation / 中文翻译 -->

# Kernel Semantic TCK v1

本目录是冻结的 `cyrene.kernel.semantic/v1` 契约可执行的兼容性基线。规范说明仍位于 `docs/contracts/kernel-semantic-contract-v1.md`；这些 TSV fixture 是其机器可读验收向量。Python、Kotlin 和 Rust 分别独立评估相同决策。

该测试套件冻结了各语言投影不得出现分歧的四个方面：

- identifier 语法、timestamp 范围和数值上限；
- 完整的 Lease、Worker 和 Operation 状态转换矩阵；
- semantic revision 协商以及精确的 Capability/Quantity 匹配；
- Lease 和 EndpointGrant 的 fencing/expiry authority；
- Event source、replay gap 和有界分页行为。

在本目录运行不依赖外部服务的参考 runner：

```text
python python/kernel_semantic_tck.py
kotlinc kotlin/KernelSemanticTck.kt -include-runtime -d kernel-semantic-tck.jar
java -jar kernel-semantic-tck.jar .
```

Rust 投影会在其单元测试中消费这些文件。语言 SDK 只有在其公开类型和 transport decoder 通过同一组向量、拒绝缺失/未知必需值且不添加较宽松的本地默认值时才符合契约。通过该套件不代表某种 transport、认证策略、scheduler 或 Provider 实现已经通过认证。

Frozen-v1 变更规则：已接受输入、状态转换、authority 决策、字段含义和数值上限均不可原地更改。任何此类变化都需要 semantic v2。只有在忽略新增字段后仍保留全部 v1 决策时，才允许新增可选投影字段。

## 覆盖矩阵

本套件目前覆盖 `docs/contracts/kernel-semantic-contract-v1.md` §11 列出的所有 v1 action 和每个 v1 `reason_code`。

### Actions（18/18）

| Action | Fixture 文件 |
|---|---|
| `NEGOTIATE` | `negotiation.tsv` |
| `REGISTER_PROVIDER` | `provider.tsv`（新增） |
| `PUBLISH_INVENTORY` | `provider.tsv`（新增） |
| `RECONCILE_PROVIDER` | `provider.tsv`（新增） |
| `ACQUIRE_LEASE` | `lease_acquire.tsv`（新增） |
| `RENEW_LEASE` | `renewal.tsv` |
| `RELEASE_LEASE` | `transitions.tsv`、`authority.tsv` |
| `START_WORKER` | `transitions.tsv`（worker 矩阵） |
| `HEARTBEAT_WORKER` | worker-control TCK `semantic_scenarios.tsv` |
| `STOP_WORKER` | `transitions.tsv`（worker 矩阵） |
| `CREATE_OPERATION` | `transitions.tsv`（operation 矩阵） |
| `REPORT_OPERATION` | `transitions.tsv`（operation 矩阵） |
| `CANCEL_OPERATION` | `transitions.tsv`（operation 矩阵） |
| `PUBLISH_ENDPOINT` | `endpoint.tsv`（新增） |
| `AUTHORIZE_ENDPOINT` | `authority.tsv`（grant 行） |
| `REVOKE_ENDPOINT` | `endpoint.tsv`（新增） |
| `READ_EVENTS` | `replay.tsv` |
| `WATCH_EVENTS` | `replay.tsv` 中的初始 replay 和连续性向量 |

### Reason Code（全部覆盖）

| 类别 | Reason Code | Fixture 文件 |
|---|---|---|
| compatibility | `CONTRACT_INCOMPATIBLE` | `negotiation.tsv` |
| authentication | `AUTHENTICATION_REQUIRED` | `denials.tsv`（新增） |
| authority | `AUTHORITY_DENIED` | `endpoint.tsv`（新增） |
| validity | `REQUIRED_FIELD_MISSING` | `denials.tsv`（新增） |
| validity | `UNKNOWN_ENUM_VALUE` | `denials.tsv`（新增） |
| validity | `TEXT_INVALID` | `identifiers.tsv` |
| validity | `NAMESPACED_ID_INVALID` | `identifiers.tsv` |
| validity | `REASON_CODE_INVALID` | `denials.tsv`（新增） |
| validity | `TIMESTAMP_INVALID` | `identifiers.tsv` |
| authority | `GENERATION_INVALID` | `provider.tsv`（新增） |
| authority | `STALE_GENERATION` | `provider.tsv`（新增） |
| authority | `FENCE_TOKEN_INVALID` | `denials.tsv`（新增） |
| authority | `FENCE_MISMATCH` | `renewal.tsv` |
| authority | `LEASE_EXPIRED` | `renewal.tsv`、`lease_acquire.tsv`（新增） |
| authority | `LEASE_NOT_ACTIVE` | `renewal.tsv` |
| authority | `LEASE_RENEWAL_INVALID` | `renewal.tsv` |
| lifecycle | `STATE_TRANSITION_INVALID` | `transitions_invalid.tsv`（新增） |
| lifecycle | `REPLAY_GAP` | `replay.tsv`（`GAP`） |
| lifecycle | `EVENT_SOURCE_CHANGED` | `replay.tsv`（`SOURCE_CHANGED`） |
