# cy-workspace-fabric

`cy-workspace-fabric` retains the legacy V1 Account/Directory boundary,
transport-neutral Workspace connection descriptor, API port, and mTLS relay
used by Platform acceptance fixtures. It is not the production Connector or
Relay process. Current Connector and Relay runtimes are owned by
`Cyrene-Plugins-Official` under `runtime/rust/`; Platform continues to own the
Authority control plane, identity, authorization, and directory storage.

The reusable outbound Workspace client transport lives in
[`../cy-workspace-client-sdk/`](../cy-workspace-client-sdk/README.md), and its
low-level HTTPS mTLS dialer lives in
[`../cy-mtls-channel-client/`](../cy-mtls-channel-client/README.md). Fabric
retains the legacy fixture's identity and authorization gates,
direct-versus-Relay selection, fallback policy, and Workspace request
dispatch.

本 crate 保留旧 V1 Account/Directory 边界、transport-neutral Workspace 连接描述符、API
port，以及 Platform 验收 fixture 使用的 mTLS Relay。它不是生产 Connector 或 Relay 进程。
当前 Connector 与 Relay runtime 由 `Cyrene-Plugins-Official` 的 `runtime/rust/` 持有；
Platform 继续持有 Authority control plane、身份、授权与 Directory storage。

可复用的 Workspace 出站 client transport 位于
[`../cy-workspace-client-sdk/`](../cy-workspace-client-sdk/README.md)，底层 HTTPS mTLS dialer 位于
[`../cy-mtls-channel-client/`](../cy-mtls-channel-client/README.md)。Fabric 继续持有
旧 fixture 的身份与授权 gate、直连/Relay 选择、fallback policy 和 Workspace request
dispatch。

It consumes the existing Execution Fabric connectivity provider and semantic
Operation identity. It does not own Product state, Lease/Fence, Runtime state,
Artifact identity/content, or user credential issuance.

它消费既有 Execution Fabric connectivity provider 与 semantic Operation
identity；不拥有 Product 状态、Lease/Fence、Runtime 状态、Artifact identity/content
或用户凭据签发。

## Layout / 目录

| Entry | Responsibility | 职责 |
| --- | --- | --- |
| `src/auth.rs` | Replaceable Frontend user-session verifier. | 可替换的 Frontend 用户会话校验器。 |
| `src/device_auth.rs` | Tonic mTLS peer-certificate facts and registry-backed Workspace connector authorization. | Tonic mTLS 对端证书事实与注册表驱动的 Workspace Connector 授权。 |
| `src/directory.rs` | Membership-scoped Workspace discovery. | 基于成员关系的 Workspace 发现。 |
| `src/device_registry.rs` | Import and revocation port for externally approved device certificates. | 外部批准设备证书的导入与撤销接口。 |
| `src/persistent_directory.rs` | Private, single-owner snapshot storage for memberships, descriptors, and device records. | 成员关系、描述符与设备记录的私有单实例快照存储。 |
| `src/api.rs` | Stable frontend-to-Workspace API port and LOCAL adapter. | 稳定 frontend API port 与 LOCAL adapter。 |
| `src/direct.rs` | Workspace API endpoint with session and membership checks. | 校验会话与成员关系的 Workspace API 直连端点。 |
| `src/frontend_relay_client.rs` | Verified-principal Web Frontend client for an outbound mTLS Relay session. | 基于已验证主体的出站 mTLS Relay 客户端。 |
| `src/relay.rs` | Live application request routing without Workspace authority. | 不拥有 Workspace 权威的实时应用请求路由。 |
| `src/transport.rs` | Connector-facing Relay session service path; client dialing is in the SDK. | Connector 侧 Relay session service path；client dialing 位于 SDK。 |
| `../cy-mtls-channel-client/` | Reusable HTTPS mTLS Tonic channel construction; no identity, authorization, routing, or dispatch authority. | 可复用的 HTTPS mTLS Tonic channel 构造；不持有身份、授权、路由或 dispatch 权限。 |
| `src/bin/` | Acceptance fixtures and restricted Directory provisioning CLI. | Acceptance fixture 与受限 Directory 配置 CLI。 |

The legacy V1 Web Frontend Relay adapter's trust boundaries are documented in
[`WEB_FRONTEND_RELAY_CLIENT.md`](WEB_FRONTEND_RELAY_CLIENT.md); it is retained
for compatibility context and does not configure the current runtime.

旧 V1 Web Frontend Relay adapter 的信任边界见
[`WEB_FRONTEND_RELAY_CLIENT.md`](WEB_FRONTEND_RELAY_CLIENT.md)；该文档仅供兼容背景参考，
不配置当前 runtime。

Product API v2 authorization and projection remain in
`cy-workspace-control-plane` and `cy-workspace-product-contracts`. The former
Platform-only generic HTTP adapter and its Connector composition were retired
with the old Platform Connector host. Current connection runtime source and
package validation belong to `Cyrene-Plugins-Official`.

Product API v2 授权与投影仍位于 `cy-workspace-control-plane` 和
`cy-workspace-product-contracts`。Platform 旧 generic HTTP adapter 与 Connector 组合已随旧
Platform Connector host 退役。当前连接 runtime 源码与 package 验证由
`Cyrene-Plugins-Official` 持有。

## Retired Platform Local Sidecar

The former Platform `cy-workspace-sidecar` process and its dedicated binary
artifact workflow have been removed. The production component with ID
`cy-workspace-sidecar` is owned by `Cyrene-Plugins-Official` at
`runtime/rust/cyrene-workspace-sidecar`; Platform's systemd unit resolves that
component through `cyrene component-run`.

The Platform client SDK and local Sidecar wire contracts remain available for
compatibility and Authority-facing APIs. Earlier Platform Sidecar profile,
credential-bundle, and runtime instructions do not configure the Plugins
process. Platform continues to own Authority identity, authorization,
WebAuthn, Directory, and PostgreSQL storage.

## Durable device authorization records

`PostgresDeviceAuthorizationStore::start_or_recover_registered` is the only
PostgreSQL entry point that creates a new device authorization. It resolves
the registration binding and writes the authorization in one transaction,
locking the Directory device identity while it checks the current generation.
The legacy `insert` and `insert_registered` store methods return unavailable;
a caller-supplied binding snapshot cannot prove that Directory and
authorization changes were atomic.

Rows created before the registration recovery digest was introduced keep a
`NULL` digest. They remain readable and can finish an already pending
certificate acknowledgement, but cannot be used for code recovery. New V4 ACK
rows retain the approver and complete issued-certificate snapshot needed for
later retirement; legacy V3 `Delivered` rows have no such snapshot and must
conflict on rotation. The store never reconstructs a missing digest or persists
raw device or user codes. New-key mTLS rotation stays unavailable until the
predecessor supersede/retirement transition and Directory generation advance
are committed together by the PostgreSQL adapter.

Before any certificate-retirement port call, the manager asks PostgreSQL to
claim the exact `RetirementPending` revision whose retry time is due. The
claim transaction checks the persisted state and revision against the primary
database clock, writes a 60-second claim lease, and advances the revision.
The lease is at least 60 seconds and extends through the current exponential
retry delay when that delay is longer.
The CA is called only after that transaction returns success; queue pressure,
storage errors, and a missing or invalid adapter timeout declaration fail
closed. An ambiguous claim timeout may leave a lease committed, so no CA call is
made and recovery waits for lease expiry. The adapter supplies a trusted hard
call-timeout declaration shorter than the 60-second minimum lease and must
enforce that timeout itself. The manager validates the declaration and checks
the elapsed time after the synchronous call returns, but cannot interrupt a
blocked call or a remote request that continues after the adapter returns. A
call that returns after its declared timeout is handled as an unknown outcome.
The CA must remain idempotent by authorization ID and certificate fingerprint,
or provide equivalent attempt fencing, so replay is safe if an earlier remote
request is still in progress. Success and failure are committed against the
claimed revision once; an older call cannot update a newer claim. A process
crash after a committed claim delays recovery until the lease expires and retry
eligibility is checked again. After an ambiguous CA result and lease expiry,
the same authorization and fingerprint may be replayed, preserving at-least-once
retirement semantics.

The device registry accepts staged and delivered authorization snapshots only
in wrapper V5 when the server-written `certificate_validation` marker matches
the exact certificate SHA-256 and Directory registration binding. Staging
requires that validation timestamp to be no more than 30 seconds old according
to the PostgreSQL clock. A retry for an already staged exact V5 snapshot is
idempotent after that window, but a new stage still requires fresh validation.
Activation and reconciliation also require a fresh marker when moving a
`pending_ack` row to `active`; an older marker leaves the durable ACK pending
until the Manager revalidates it and retries. Repeated activation of an already
active row remains idempotent. Historically, the retired Platform V1 Relay
dispatch fence checked the active registry and identity tuple, and that host
independently checked current certificate revocation on every request. This
documents the former Platform host only; it is not evidence that the Plugins
Relay exposes or enforces the same path.
Registry migration `0003_certificate_validation_provenance` refuses to proceed
while legacy `active` or `pending_ack` rows lack matching V5 provenance. It
leaves those rows unchanged for controlled audit and recovery, and requires the
SQLx transactional migration runner at `READ COMMITTED` isolation so its
writer-blocking table lock spans a fresh preflight and gate replacement.
Legacy V1–V4 Delivered rows cannot be activated by Registry; V1–V3 also lack
the certificate snapshot needed for automatic revalidation.

调用证书撤销 port 前，manager 会先请求 PostgreSQL claim 到期的精确
`RetirementPending` revision。claim 事务使用主库时钟校验已持久化的状态和版本，写入
至少 60 秒租约（如当前指数退避更长，则延至该退避时间）并推进 revision。只有事务明确成功返回后才调用 CA；队列拥塞、存储错误和适配器
硬请求超时都会 fail closed。claim 超时结果不确定时可能已留下租约，因此不会调用 CA，恢复
流程会等待租约过期。适配器提供受信的硬调用超时声明，manager 会校验它短于 60 秒最短租约，缺失或无效时
拒绝发起 CA 调用；适配器仍须自行执行该超时。manager 只能在同步调用返回后检查耗时，无法中断挂起调用或
适配器返回后仍在执行的远端请求。超过声明时限才返回的调用按结果不确定处理。如果早先远端请求仍在执行，
CA 必须按 authorization ID 和证书指纹幂等，或提供等效的 attempt fencing，以保证重放安全。成功和失败只允许
基于 claimed revision 提交一次，旧调用不能更新较新的 claim。已提交 claim 后进程崩溃时，要等租约到期再检查重试资格。
CA 结果不确定且租约到期后，可能会以相同 authorization ID 和证书指纹重放；整个流程保持至少一次撤销语义。

`PostgresUserCodeAttemptReservation` provides the shared asynchronous limiter
reservation used by the enrollment composite before it calls the synchronous
state machine. It stores only the caller-derived 32-byte abuse digest, a
database-clock window start, an attempt count, and that digest's window length
and maximum. Expiry cleanup uses each row's persisted policy; presenting the
same digest with different parameters while its stored window is active fails
closed rather than resetting or relaxing its limit. Once that stored window
expires by database time, the row can be reused under the new policy. A
PostgreSQL transaction lock makes updates and the 4,096-active-key ceiling
consistent across service instances. This global lock trades write throughput
for a simple shared bound;
the adapter admits at most 32 in-flight reservations and rejects excess load.
`Ok(false)` refuses the request without incrementing beyond the configured
maximum, matching the existing in-memory port. Timeout, database errors,
invalid bounds, and capacity exhaustion fail closed. The composite must pass
the exact same digest/window/maximum to its request-scoped one-shot
`UserCodeAttemptLimiter`, and must not call `block_on` from a Tokio worker.

The limiter shares the device-authorization database, TLS policy, migration
history, and `cyrene_workspace_device_authorization_app` runtime role. Migration
`0005_user_code_attempt_limiter` grants that role access only to the new
attempt-window table; no raw IP address, pre-auth session, user code, or other
identity value is stored. Run the existing device-authorization migrator with
`CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL` before deploying
the composite. Runtime connections continue to use
`CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL` and PostgreSQL
`sslmode=verify-full`.

## Retired Platform connection hosts

The standalone Platform Connector and Relay hosts described by earlier V1
notes have been removed with their source packages and image build paths.
`cy-workspace-fabric` remains for legacy V1 compatibility fixtures; it is not a
production Connector or Relay runtime. Current connection runtime packages are
owned by `Cyrene-Plugins-Official` under `runtime/rust/cyrene-workspace-connector`
and `runtime/rust/cyrene-workspace-relay`.

Platform continues to own the Authority control plane, identity, authorization,
WebAuthn, Directory, and PostgreSQL storage. Historical Platform host probes
under `tooling/acceptance/native-workspace-v2/` are marked retired and do not
provide acceptance evidence for the current Plugins runtime.

## PostgreSQL authority and Directory ports

`PostgresWorkspaceDirectory` provides async reads of memberships, assigned
Product roles, and published descriptors. `organizations_for_verified_identity`
accepts an issuer/subject pair supplied by a separate trusted OIDC verifier and
returns the distinct organization candidates; callers reject zero or multiple
candidates. The strict convenience method `organization_for_verified_identity`
returns an error unless there is exactly one organization. This store does not
validate OIDC tokens or create memberships from identity claims. It implements
the object-safe async `WorkspaceDirectory` port. The retained Platform
Authority and Web BFF await its reads while preserving storage errors separately
from non-membership, without `block_on` or synchronous database calls. The
earlier Platform Relay-host composition used this port and was removed; these
settings do not configure the Plugins-owned Relay. The database server,
credentials, TLS trust, network path, and deployment are not provisioned by
this crate or the current ACA template.

The migrations create `memberships`, `roles`, `descriptors`, append-only
`audit_events`, and stable device-registration binding tables. Operator grant/revoke operations write their changes and
audit rows in one database transaction. Revoking a membership cascades its
roles and records the removed roles in that transaction. Only the five current
Product user-command roles can be assigned; membership and workload roles come
from separate authorities. The audit table rejects UPDATE and DELETE, and the
reader role has SELECT-only access. The operator role has column-level UPDATE
on `memberships.provisioned_at`, which PostgreSQL requires to use
`SELECT FOR UPDATE` on membership rows; it has no UPDATE privilege on other
membership columns.

Configure four separate trusted process URLs:

* `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`: runtime reader login, a member of
  `cyrene_workspace_directory_reader` only.
* `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL`: local provisioning CLI
  login, a member of `cyrene_workspace_directory_operator`.
* `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`: deployment migration
  login with the database privileges needed to create roles and schema.
* `CYRENE_WORKSPACE_DEVICE_REGISTRATION_DATABASE_URL`: enrollment binding
  login, a member only of `cyrene_workspace_device_registrar`; it cannot mutate
  memberships, Product roles, descriptors, or operator audit rows.

All connections require PostgreSQL TLS with certificate and hostname
verification (`sslmode=verify-full`); configure `sslrootcert` when using a
private database CA. `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID` supplies the
operator audit actor from trusted process configuration. The CLI does not accept
an actor ID from command arguments. Keep the operator and migration URLs out of
the service runtime. Database users, role memberships, secrets, TLS, backups,
and deployment are provisioned outside this crate.

Example local provisioning commands:

```bash
cy-workspace-directory-admin migrate
cy-workspace-directory-admin grant-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --role workspace.product.command.catalyst.create_dataset.v1 --reason TICKET
cy-workspace-directory-admin revoke-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --reason TICKET
```

The down migrations drop the Directory and device-registration tables. The
registration adapter binds a client-generated, cryptographically random
256-bit recovery credential (stored only as a domain-separated SHA-256 digest) to exact scope, CSR/SPKI digests, stable
`device_id`, and authorization generation. Exact retries return the original
binding; changing any bound value conflicts. A new key can reuse a device ID
only when a trusted caller supplies the current mTLS-authenticated device key.
This adapter does not enable enrollment start or approval: production must
combine binding/generation and authorization-state CAS in one PostgreSQL
transaction while locking the device identity row. Relay/Direct PostgreSQL
deployment, credentials, OIDC verification, and production host configuration
remain external work; this is not a production cutover.

Run:

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
---

<!-- Chinese Translation / 中文翻译 -->

# cy-workspace-fabric

`cy-workspace-fabric` 保留旧 V1 Account/Directory 边界、transport-neutral Workspace
connection descriptor、API port，以及 Platform 验收 fixture 使用的 mTLS relay。它不是生产
Connector 或 Relay 进程。当前 Connector 与 Relay runtime 由 `Cyrene-Plugins-Official` 的
`runtime/rust/` 持有；Platform 继续持有 Authority control plane、身份、授权与 Directory storage。

它使用既有 Execution Fabric connectivity provider 和 semantic Operation identity。不拥有 Product 状态、Lease/Fence、Runtime 状态、Artifact identity/content 或用户凭据签发。

## 目录结构

| 条目 | 职责 |
|---|---|
| `src/auth.rs` | 可替换的 Frontend 用户会话校验器。 |
| `src/device_auth.rs` | Tonic mTLS 对端证书事实与注册表驱动的 Workspace Connector 授权。 |
| `src/directory.rs` | 以成员关系为作用域的 Workspace 发现。 |
| `src/device_registry.rs` | 外部批准设备证书的导入与撤销接口。 |
| `src/persistent_directory.rs` | 成员关系、描述符与设备记录的私有单实例快照存储；需挂载持久卷。 |
| `src/api.rs` | 稳定的 frontend-to-Workspace API port 与 LOCAL adapter。 |
| `src/direct.rs` | 校验会话与成员关系的 Workspace API 私网直连端点。 |
| `src/relay.rs` | 不拥有 Workspace authority 的实时应用请求路由。 |
| `src/transport.rs` | Connector 侧 Relay session service path；client dialing 位于 SDK。 |
| `../cy-mtls-channel-client/` | 可复用的 HTTPS mTLS Tonic channel 构造，不拥有身份、授权、路由或 dispatch authority。 |
| `src/bin/` | 验收 fixture 与受限 Directory 配置 CLI。 |

## 已退役的 Platform Local Sidecar

Platform 旧 `cy-workspace-sidecar` 进程及其专属 binary artifact workflow 已移除。组件 ID
`cy-workspace-sidecar` 对应的生产组件由 `Cyrene-Plugins-Official` 持有，源码位于
`runtime/rust/cyrene-workspace-sidecar`；Platform systemd unit 通过
`cyrene component-run` 按组件 ID 解析该组件。

Platform client SDK 与 local Sidecar wire contract 仍供兼容及 Authority API 使用。早期 Platform
Sidecar 的 profile、凭据 bundle 和运行说明不适用于 Plugins 进程。Platform 继续持有 Authority
身份、授权、WebAuthn、Directory 与 PostgreSQL storage。

## 设备授权的 PostgreSQL 持久记录

PostgreSQL 中只有 `PostgresDeviceAuthorizationStore::start_or_recover_registered`
可以创建新的设备授权。它在一个事务内解析注册绑定并写入授权，同时锁定 Directory
设备身份并检查当前世代。旧 `insert` 与 `insert_registered` store 方法均返回
unavailable；调用方传入的绑定快照不能证明 Directory 更新与授权写入具备原子性。

在注册恢复摘要引入前创建的记录保留 `NULL` 摘要。它们仍可读取，也可完成已经待处理的
证书确认，但不能用于 code 恢复或设备轮换。存储层不会补造缺失摘要，也不会持久化明文
device/user code。在旧记录安全地进入 supersede/retirement 且同事务实现前，使用新 key 的
mTLS 设备轮换保持不可用。

设备注册表只接受带有 V5 wrapper 和服务端写入的
`certificate_validation` 标记的 staged/Delivered 授权快照；标记中的精确证书
SHA-256 和 Directory registration binding 必须匹配。Stage 要求 PostgreSQL
时钟下的验证时间不超过 30 秒。已精确持久化的 V5 stage 重试在该时限后仍保持幂等，
新 stage 仍要求新鲜验证。`pending_ack` 转为 `active` 时，Activation/reconcile 也要求
验证标记新鲜；较早的标记会让持久 ACK 保持待处理，直到 Manager 重新验证、刷新标记并重试。
对已 active 行重复激活仍保持幂等。历史上的 Platform V1 Relay dispatch fence 会校验
active registry 与身份元组，且该旧宿主会在每次请求上独立检查当前证书撤销状态。这只记录已退役
Platform 宿主的行为，不证明 Plugins Relay 提供或执行相同路径。Registry 迁移
`0003_certificate_validation_provenance` 遇到缺少匹配 V5 来源证明的旧
`active` 或 `pending_ack` 行时会拒绝迁移，不会修改这些审计行，需在受控窗口盘点并恢复。
迁移要求使用 `READ COMMITTED` 隔离级别下的 SQLx transactional runner，使阻止写入的表锁覆盖
基于新快照的旧行预检与 gate 替换。
V1–V4 的 Delivered 记录不能由 Registry 激活；V1–V3 也缺少可供自动复验的证书快照。

`PostgresUserCodeAttemptReservation` 为 enrollment composite 提供共享异步尝试预留；composite
在调用同步状态机前先预留。它只保存调用方派生的 32-byte abuse digest、数据库时钟窗口起点、计数、
该 digest 的窗口长度和次数上限。过期清理由每行持久化的策略决定；同一 digest 使用不同参数时 fail closed，
在旧窗口仍有效时不会重置或放宽限制；按数据库时钟确认旧窗口到期后，可用新策略复用该行。
PostgreSQL transaction lock 让多实例共享固定窗口与 4,096 个活跃 digest 上限；
超时、数据库错误、
无效边界或容量耗尽均 fail closed。全局 lock 以写入吞吐换取简单共享上限；adapter 最多接收 32 个
并发预留，超出后拒绝。`Ok(false)` 表示拒绝本次请求，并按现有 in-memory port 语义不再递增已达上限的计数。
Composite 必须把完全相同的 digest/window/maximum 传给本次请求的 one-shot `UserCodeAttemptLimiter`，
且不得在 Tokio worker 中调用 `block_on`。

Limiter 与设备授权共用数据库、TLS 策略、迁移历史及
`cyrene_workspace_device_authorization_app` runtime role。迁移
`0005_user_code_attempt_limiter` 只向该 role 授予新尝试窗口表的访问权；不会存储原始 IP、pre-auth
session、user code 或其他身份值。部署 composite 前，使用
`CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL` 运行现有设备授权迁移。Runtime
仍使用 `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL`，并强制 PostgreSQL `sslmode=verify-full`。

## 已退役的 Platform 连接宿主

早期 V1 文档描述的 Platform 独立 Connector 与 Relay host 已随源码 package
和镜像构建入口一起移除。`cy-workspace-fabric` 仅为旧 V1 兼容 fixture 保留；它不提供
生产 Connector 或 Relay runtime。当前连接 runtime package 由 `Cyrene-Plugins-Official`
持有，路径为 `runtime/rust/cyrene-workspace-connector` 与
`runtime/rust/cyrene-workspace-relay`。

Platform 继续持有 Authority control plane、身份、授权、WebAuthn、Directory 与
PostgreSQL storage。`tooling/acceptance/native-workspace-v2/` 下标记为退役的旧 Platform
host probe 不构成当前 Plugins runtime 的验收证据。

## PostgreSQL 权威存储与 Directory 接口

`PostgresWorkspaceDirectory` 提供成员关系、已分配 Product 角色和已发布 descriptor 的异步读取。`organizations_for_verified_identity` 只接收独立可信 OIDC verifier 提供的 issuer/subject 并返回候选 organization；无候选或多个候选由调用方拒绝。严格便利方法 `organization_for_verified_identity` 在无映射或多 organization 时返回错误。此存储不验证 OIDC token，也不根据身份声明自动创建成员关系。它实现 object-safe async `WorkspaceDirectory` port。保留的 Platform Authority 与 Web BFF await 目录读取，并区分存储错误与非成员，不使用 `block_on` 或同步数据库调用。早期 Platform Relay-host 组合曾使用该 port，现已移除；这些设置不配置 Plugins 所有的 Relay。数据库服务、凭据、TLS 信任、网络路径和部署不会由本 crate 或当前 ACA 模板创建。

迁移创建 `memberships`、`roles`、`descriptors` 和只追加的 `audit_events` 表。Operator 授权/撤销与审计行在同一数据库事务中提交；撤销成员时级联删除其角色，并在同一事务记录被移除角色。仅允许分配当前五种 Product 用户命令角色；成员标记和 workload 角色由其他 authority 管理。审计表拒绝 UPDATE/DELETE，reader 角色仅有 SELECT 权限。
Operator role 仅对 `memberships.provisioned_at` 具有列级 UPDATE 权限，这是 PostgreSQL 对成员行使用 `SELECT FOR UPDATE` 的要求；其不能 UPDATE 其他成员列。

使用四个独立的可信运行时 URL：

* `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`：服务 reader 登录，只加入 `cyrene_workspace_directory_reader`。
* `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL`：本地 provisioning CLI 登录，只加入 `cyrene_workspace_directory_operator`。
* `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`：部署迁移登录，需有创建角色和 schema 的数据库权限。
* `CYRENE_WORKSPACE_DEVICE_REGISTRATION_DATABASE_URL`：enrollment binding 登录，只加入 `cyrene_workspace_device_registrar`，不能修改成员、Product role、descriptor 或 operator audit。

所有连接均强制 PostgreSQL TLS 证书与主机名校验（`sslmode=verify-full`）；私有数据库 CA 需配置 `sslrootcert`。`CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID` 从可信进程配置提供审计 actor，CLI 参数不接受 actor ID。服务进程不得使用 operator 或 migration URL。数据库用户、角色成员授权、secret、TLS、备份和部署由 crate 外部管理。

本地 provisioning 示例：

```bash
cy-workspace-directory-admin migrate
cy-workspace-directory-admin grant-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --role workspace.product.command.catalyst.create_dataset.v1 --reason TICKET
cy-workspace-directory-admin revoke-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --reason TICKET
```

down migration 会删除 Directory 与设备 registration 表。Registration adapter 将客户端使用 CSPRNG 生成的 256-bit recovery credential（仅以 domain-separated SHA-256 digest 存储）绑定到精确 scope、CSR/SPKI digest、稳定 `device_id` 与 authorization generation；完全相同的重试返回原绑定，任一绑定值变化都会冲突。只有可信调用方提供当前通过 mTLS 认证的设备 key 时，新 key 才能复用 device ID。此 adapter 不启用 enrollment start 或 approval：生产必须在锁定设备身份行的同一 PostgreSQL transaction 中组合 binding/generation 与授权状态 CAS。Relay/Direct PostgreSQL 部署、凭据、OIDC verifier 和 production host 配置仍属于外部工作，尚未完成生产切换。

运行：

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
