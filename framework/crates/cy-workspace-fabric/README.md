# cy-workspace-fabric

`cy-workspace-fabric` provides the product-neutral Account/Directory boundary,
transport-neutral Workspace connection descriptor, stable Workspace API port,
an authenticated private mTLS direct endpoint, and the outbound mTLS
application relay used by the v1 reference vertical. Candidate selection tries
`LAN_DIRECT` before `RELAY` according to descriptor priority.

本 crate 提供产品无关的 Account/Directory 边界、transport-neutral Workspace
连接描述符、稳定 Workspace API port、私网 mTLS 直连端点与 v1 参考纵向使用的出站
mTLS 应用层 Relay。候选选择按描述符优先级先尝试 `LAN_DIRECT`，再选择 `RELAY`。

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
| `src/transport.rs` | Direct candidate selection and outbound mTLS Relay transport. | 直连候选选择与出站 mTLS Relay 传输。 |
| `src/bin/` | Fail-closed Relay host and acceptance-only connector/reference frontend fixtures. | fail-closed Relay host 与仅用于验收的 Connector/参考 frontend fixture。 |
| `src/sidecar.rs` | Loopback-authenticated bridge for non-Rust Workspace clients. | 为非 Rust Workspace client 提供 loopback 认证代理。 |
| `src/bin/` | Acceptance relay/connector/frontend fixtures and the loopback sidecar executable. | 验收 Relay/Connector/frontend fixture 与 loopback sidecar 可执行程序。 |

The Web Frontend Relay adapter's trust boundaries and required host wiring are
documented in [`WEB_FRONTEND_RELAY_CLIENT.md`](WEB_FRONTEND_RELAY_CLIENT.md).

Web Frontend Relay adapter 的信任边界与 host 接线要求见
[`WEB_FRONTEND_RELAY_CLIENT.md`](WEB_FRONTEND_RELAY_CLIENT.md)。

The production outbound Connector host's fixed private-file layout, exact
device identity construction, Product scope checks, and external Relay registry
gate are documented in [`WORKSPACE_CONNECTOR_HOST.md`](WORKSPACE_CONNECTOR_HOST.md).

生产出站 Connector host 的固定私有文件布局、设备身份构造、Product scope 校验与
Relay 外部注册表 gate 见 [`WORKSPACE_CONNECTOR_HOST.md`](WORKSPACE_CONNECTOR_HOST.md)。

## Local Python bridge

`cy-workspace-sidecar` exposes the versioned local gRPC contract in
`cyrene.workspace.local.v1` on `127.0.0.1:41680` by default. It accepts only a
local `Authorization: Bearer ...` token, returns Workspace IDs and display
names without private route candidates, and forwards the existing generic
Workspace Operation API. The bind address is fixed to IPv4 loopback; only the
port can be changed with `CYRENE_WORKSPACE_SIDECAR_PORT`.

The Python service must share the sidecar's network namespace so its own
`127.0.0.1` reaches this listener. A host loopback binding is not reachable from
an unrelated container namespace.

Python clients should generate their gRPC stub from
`contracts/proto/cyrene/workspace/local/v1/workspace_sidecar.proto` and attach
`authorization: Bearer <local-token>` metadata to every RPC.

The sidecar never issues identity. A trusted credential/profile provisioner
must place the externally issued session credential, user identity, and local
allowlist in a protected JSON bundle, with the client key at an absolute path:

```json
{
  "relay_endpoint": "https://relay.example.invalid",
  "relay_server_name": "relay.example.invalid",
  "relay_ca_certificate_file": "/run/secrets/relay-ca.pem",
  "client_certificate_file": "/run/secrets/workspace-client.pem",
  "client_key_file": "/run/secrets/workspace-client-key.pem",
  "session_credential": "<short-lived issuer-provided credential>",
  "user_issuer": "https://identity.example.invalid",
  "user_subject": "<issuer-provided subject>",
  "organization_id": "<issuer-provided organization>",
  "allowed_workspace_ids": ["<deployment-approved-workspace>"],
  "allowed_operations": ["get_operation", "start_operation"]
}
```

Set `CYRENE_WORKSPACE_SIDECAR_CREDENTIAL_BUNDLE` and
`CYRENE_WORKSPACE_SIDECAR_LOCAL_TOKEN_FILE` to absolute paths. On Unix, the
credential bundle and client key must be mode `0600` or `0400`; the local token
may be `0640` or `0440` when its group contains only the sidecar and authorized
Python client. Generate the local bearer independently as a high-entropy secret;
never reuse or derive it from a Relay session or device-enrollment credential.
Startup and every bundle reload reject a local bearer equal to the current Relay
session credential. The `allowed_workspace_ids` and `allowed_operations` fields are
deployment policy: keep them scoped to one sidecar instance and never use `*`.
Python services sharing a local token share that instance's full allowlist, so
use a separate sidecar profile/token for each service trust boundary. The local
token must contain at least 32 ASCII letters, digits, dots,
underscores, or hyphens. Missing, malformed, or overly accessible secrets make
the process fail closed. Credentials are re-read on each remote call, so an
issuer can rotate the bundle through an atomic file replacement. The local
bearer token is read at startup and changes require a restart.

`Health` reports local configuration/listener readiness only, not Relay health.
Each `Execute` call discovers the requested Workspace and selects the current
`LAN_DIRECT` or `RELAY` candidate. The local API currently forwards only the
generic Start/Get Operation contract; it does not define Product-specific
Python APIs, which still require the Phase 3 Workspace projections.

`FileWorkspaceDirectory::open` needs a private directory on a persistent volume
with one active owner. `replace` publishes a complete membership and descriptor
revision atomically while preserving device records. The registry imports a
certificate record only after external approval and issuance have completed;
the imported authorization state is `Approved`. It stores a scoped identity
and a supplied SHA-256 certificate fingerprint, supports lookup by device key
or fingerprint, rejects duplicate identities and fingerprints, and allows
revocation. Revoked identities and fingerprints cannot be re-imported; a
replacement certificate needs a future explicit rotation operation. The
registry does not represent pending requests or perform approval decisions.

`RegistryWorkspaceDeviceVerifier` authorizes Relay `WorkspaceConnector`
participants from the leaf certificate returned by Tonic's TLS peer metadata.
Tonic's server TLS layer validates the peer chain; this adapter computes a
lowercase SHA-256 fingerprint over the leaf DER, reads its `notAfter` time, and
requires a matching `Approved` registry record that matches the RelayHello
organization, Workspace, and device IDs. RelayHello `user`,
`session_credential`, and `enrollment_state` do not establish connector
identity. `WorkspaceRelay::new` leaves connector authentication disabled;
callers must explicitly provide a device registry with
`with_workspace_device_registry`. Frontend Relay sessions and the direct
Workspace endpoint continue to use user-session authentication and membership
checks.

This certificate authenticates the Workspace connector to the Relay; it does
not authenticate the Relay service to the connector. A Workspace receiver may
trust forwarded `caller_roles` only on a Relay session whose server certificate
chains to its configured Relay CA and matches the configured server name. The
outbound transport verifies those server facts independently from the
connector's registered client certificate. The Direct endpoint remains a
Frontend user-session path and must not use a Workspace device certificate as
its user identity.

The registry store validates fingerprint syntax but does not parse
certificates, verify TLS peers, or issue credentials. The PostgreSQL
authorization store persists issued certificate DER and CA-chain bytes in its
versioned state payload for delivery acknowledgement and retirement recovery.
It never stores device private keys, raw enrollment codes, or session
credentials, and this crate does not itself run a Directory service.

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

## Relay host security state

`cy-workspace-relay-host` is the non-fixture process entrypoint around
`WorkspaceRelay`. Startup requires `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`
and connects to the read-only PostgreSQL Directory; it has no file-backed
fallback. PostgreSQL TLS uses certificate and hostname verification. Configure
`sslrootcert` for a private database CA. Missing or unavailable database
configuration fails startup, and database errors are mapped to fixed safe
errors.

Frontend authentication is deny-all unless all five BFF trust settings are
configured. When configured, the Relay checks the same RPC's ACA-overwritten
XFCC against a dedicated BFF CA, exact subject, and active fingerprint pin
before verifying the short-lived signed handoff. The browser's AAD access token
is not the Relay credential. WorkspaceConnector authentication remains
disabled: this host does not load a device CA or accept Connector sessions
until the durable device registry adapter is composed. Directory
administration, DeviceAuthorization, certificate issuance, WebAuthn, and
Product private access are not composed.

Product HTTP is performed by `ProductHttpApiAdapter` on the WorkspaceConnector
path. Frontend Relay authentication alone does not create a Product request
path; this host rejects Connector sessions, so no Product request can currently
reach its owner through this host.

`GET /healthz` reports process liveness. `GET /readyz` performs a bounded,
read-only PostgreSQL Directory probe and returns fixed dependency booleans; it
remains HTTP 503 until every required service and the deployment topology are
verified. The host cannot infer live ACA ingress configuration or private
network reachability from configuration values. This is a fail-closed
composition stage, not a production Relay deployment. See
`src/bin/README.md` for exact runtime settings and health behavior.

Run locally with the trusted database URL in the environment:

```bash
export CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL='postgresql://<reader>@<host>/<database>?sslmode=verify-full&sslrootcert=/path/to/postgres-roots.pem'
cargo run --locked -p cy-workspace-fabric --bin cy-workspace-relay-host
```

The default listeners are `127.0.0.1:8080` for gRPC and `127.0.0.1:8081` for
health probes. A non-loopback Relay bind requires
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`. A
configured Frontend requires the same explicit assertion even with loopback
binding. A non-loopback health bind requires
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`. These
settings are operator assertions, not proof of live ACA configuration.

ACA must use private HTTP/2 ingress on port 8080 with
`clientCertificateMode: require`, `allowInsecure: false`, and no direct path
around Envoy. Health probes use port 8081, which must not be exposed as ingress;
`/healthz` is liveness and `/readyz` is readiness. ACA ingress requires a client
certificate before the participant role is known, after which the Relay picks
the role-specific BFF path. It trusts XFCC only when ACA overwrites incoming
values and direct bypass is prevented. The review-only ACA template remains
unapplied. Product APIs currently use internal East Asia ACA endpoints while
the East Asia environment has no VNet; the separate West US 2 VNet environment
is not connected, and no PostgreSQL Flexible Server is provisioned. A reviewed
private network topology that reaches both Directory and the owning Product
services is a production prerequisite.

The Relay transports Workspace requests only. Product, Kernel/Lease, Runtime,
and Artifact authorities stay in their owning components; this host does not
load fixture handlers or acquire their state.

## PostgreSQL authority (async Directory port)

`PostgresWorkspaceDirectory` provides async reads of memberships, assigned
Product roles, and published descriptors. `organizations_for_verified_identity`
accepts an issuer/subject pair supplied by a separate trusted OIDC verifier and
returns the distinct organization candidates; callers reject zero or multiple
candidates. The strict convenience method `organization_for_verified_identity`
returns an error unless there is exactly one organization. This store does not
validate OIDC tokens or create memberships from identity claims. It implements
the object-safe async `WorkspaceDirectory` port, and Relay and Direct await each
read while preserving storage errors separately from non-membership. The
adapter can be injected into those services without `block_on` or synchronous
database calls. The Relay host uses this async port and requires
`CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`; there is no file-backed fallback.
The database server, credentials, TLS trust, network path, and deployment are
not provisioned by this crate or the current ACA template.

The migrations create `memberships`, `roles`, `descriptors`, append-only
`audit_events`, and stable device-registration binding tables. Operator grant/revoke operations write their changes and
audit rows in one database transaction. Revoking a membership cascades its
roles and records the removed roles in that transaction. Only the five current
Product user-command roles can be assigned; membership and workload roles come
from separate authorities. The audit table rejects UPDATE and DELETE, and the
reader role has SELECT-only access.

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

`cy-workspace-fabric` 提供与 Product 无关的 Account/Directory 边界、transport-neutral Workspace connection descriptor、稳定 Workspace API port，以及 v1 参考纵向使用的出站 mTLS application relay。

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
| `src/transport.rs` | 直连候选选择、出站 mTLS Relay client 和 connector session。 |
| `src/bin/` | fail-closed Relay host 与仅用于 acceptance 的 Connector/参考 frontend fixture。 |
| `src/sidecar.rs` | 消费外部凭据并提供本地认证 gRPC 代理。 |
| `src/bin/` | 验收 fixture 与独立 Workspace sidecar。 |

## 本地 Python 代理

`cy-workspace-sidecar` 默认在 `127.0.0.1:41680` 提供版本化本地 gRPC 合同
`cyrene.workspace.local.v1`。每个调用都必须携带单独配置的 local bearer token；代理只返回
Workspace ID 与显示名称，不向 Python 暴露私有路由候选，并转发现有的通用 Workspace
Operation API。监听地址固定为 IPv4 loopback，只能通过
`CYRENE_WORKSPACE_SIDECAR_PORT` 调整端口。Python service 必须与 sidecar 共享 network
namespace，才能通过自己的 `127.0.0.1` 访问；独立 container namespace 无法访问宿主机 loopback。

Sidecar 不签发身份。受信任的 credential/profile provisioner 必须把外部签发的 session
credential、user identity 与本地 allowlist 写入受保护的 JSON bundle，并通过绝对路径设置
`CYRENE_WORKSPACE_SIDECAR_CREDENTIAL_BUNDLE` 和
`CYRENE_WORKSPACE_SIDECAR_LOCAL_TOKEN_FILE`；bundle 示例字段见英文部分。Unix 上 bundle 与 client key 权限必须为
`0600` 或 `0400`；若 local token 使用 `0640` 或 `0440`，共享组只能包含 sidecar 与获准的 Python
client。`allowed_workspace_ids` 与 `allowed_operations` 是部署策略，应限制在单个 sidecar
instance 内，不能使用 `*`。local bearer 必须独立生成并具有高熵，不能复用或派生自 Relay
session 或 device enrollment credential；启动时及每次 bundle reload 都会拒绝与当前 Relay
session credential 相同的 local bearer。共享 local token 的 Python service 共用该实例的完整 allowlist；
不同的 service trust boundary 应使用不同的 sidecar profile/token。权限不安全、凭据缺失或格式错误都会让进程 fail closed。每次远端调用都会重新读取
bundle，因此外部 issuer 可以通过原子替换文件轮换凭据；local bearer token 在启动时读取，变更后
需重启 sidecar。

`Health` 只报告本地 listener/config 状态，不代表 Relay 可达。每次 `Execute` 都会重新发现
Workspace，并按当前 descriptor 在 `LAN_DIRECT` 与 `RELAY` 中选路。当前代理仅转发通用
Start/Get Operation contract；Product 专属 Python API 仍需 Phase 3 Workspace projection。
Python client 应基于
`contracts/proto/cyrene/workspace/local/v1/workspace_sidecar.proto` 生成 gRPC stub，并为每个
RPC 附加 `authorization: Bearer <local-token>` metadata。

`FileWorkspaceDirectory::open` 需要挂载在持久卷上的私有目录，同一时间仅允许一个实例持有。`replace` 原子发布成员关系和描述符版本，并保留设备记录。注册表只导入已经外部批准并签发的证书记录，导入时状态为 `Approved`；记录作用域身份和调用方提供的 SHA-256 证书指纹，支持按设备键或指纹查找，拒绝重复身份/指纹，并允许撤销。撤销后的身份和指纹不能重新导入；证书轮换需由未来的显式操作处理。注册表不表示待审批请求，也不执行审批决策。

`RegistryWorkspaceDeviceVerifier` 只从 Tonic TLS 对端元数据取得叶子证书，由 Tonic 服务端 TLS 层先验证证书链；适配器再计算 DER 的小写 SHA-256 指纹、读取 `notAfter`，并要求指纹匹配已批准的注册表记录，同时校验 RelayHello 中的组织、Workspace 和设备 ID。RelayHello 的 `user`、`session_credential` 和 `enrollment_state` 不构成 Connector 身份。`WorkspaceRelay::new` 默认关闭 Connector 认证；调用方必须显式通过 `with_workspace_device_registry` 提供设备注册表。Frontend Relay 会话和 Workspace 直连端点仍使用用户会话认证与成员关系校验。

此证书只认证 Workspace Connector 到 Relay 的客户端身份，不认证 Relay 服务到 Connector 的服务端身份。Workspace 接收端只能在 Relay 会话的服务端证书由配置的 Relay CA 验证且匹配配置的 server name 时信任转发的 `caller_roles`。出站 transport 会独立校验这些服务端事实与 Connector 已注册客户端证书。Direct 端点仍是 Frontend 用户会话路径，不得把 Workspace 设备证书用作用户身份。

注册表只校验指纹格式，不解析证书、不验证 TLS 对端、不签发凭据。PostgreSQL 授权存储会在版本化状态载荷中保存签发证书的 DER 与 CA 链，以支持交付确认和撤销恢复；它不会保存设备私钥、明文注册 code 或 session credential，crate 也不自行运行 Directory 服务。

## 设备授权的 PostgreSQL 持久记录

PostgreSQL 中只有 `PostgresDeviceAuthorizationStore::start_or_recover_registered`
可以创建新的设备授权。它在一个事务内解析注册绑定并写入授权，同时锁定 Directory
设备身份并检查当前世代。旧 `insert` 与 `insert_registered` store 方法均返回
unavailable；调用方传入的绑定快照不能证明 Directory 更新与授权写入具备原子性。

在注册恢复摘要引入前创建的记录保留 `NULL` 摘要。它们仍可读取，也可完成已经待处理的
证书确认，但不能用于 code 恢复或设备轮换。存储层不会补造缺失摘要，也不会持久化明文
device/user code。在旧记录安全地进入 supersede/retirement 且同事务实现前，使用新 key 的
mTLS 设备轮换保持不可用。

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

## Relay host 安全状态

`cy-workspace-relay-host` 是围绕 `WorkspaceRelay` 的非 fixture 进程入口。启动必须设置
`CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL` 并连接只读 PostgreSQL Directory；host 不再回退到文件存储。
PostgreSQL TLS 会校验证书和主机名；私有数据库 CA 需要通过 `sslrootcert` 配置。数据库配置缺失或不可用时启动失败，并将数据库错误转换为固定安全错误。

未完整配置五项 BFF 信任设置时，Frontend 认证默认拒绝。配置后，Relay 会在同一 RPC 上先使用独立 BFF CA、精确 subject 和未撤销指纹 pin 验证 ACA 覆盖写入的 XFCC，再验证短时签名 handoff。浏览器 AAD access token 不是 Relay credential。此 host 仍禁用 WorkspaceConnector：持久设备注册表 adapter 接通前，不加载 device CA，也不接受 Connector session。Directory 管理、DeviceAuthorization、证书签发、WebAuthn 和 Product 私有访问仍未组合。Product HTTP 由 WorkspaceConnector 路径上的 `ProductHttpApiAdapter` 发起；Frontend Relay authentication 不会自动创建 Product 请求通路。此 host 拒绝 Connector session，因此当前没有请求能经它到达 Product owner。

`GET /healthz` 表示进程存活。`GET /readyz` 会在有界时间内执行只读 PostgreSQL Directory 探测，并返回固定依赖布尔值；全部必需服务和部署拓扑验证完成前，保持 HTTP 503。host 无法从配置值推断 ACA 实际 ingress 设置或私网连通性。这是 fail-closed 的分阶段组合，不是生产 Relay 部署。准确运行配置和探针行为见 `src/bin/README.md`。

本地运行时，在环境变量中提供可信数据库 URL：

```bash
export CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL='postgresql://<reader>@<host>/<database>?sslmode=verify-full&sslrootcert=/path/to/postgres-roots.pem'
cargo run --locked -p cy-workspace-fabric --bin cy-workspace-relay-host
```

默认 gRPC listener 为 `127.0.0.1:8080`，健康探针 listener 为
`127.0.0.1:8081`。Relay 使用非 loopback bind 时必须设置
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`；配置 Frontend 时，即使 loopback bind 也需要该显式声明。健康 listener 使用非 loopback bind 时必须设置
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`。这些配置只是运维声明，不能证明 ACA 实际设置。

ACA 必须使用 8080 私有 HTTP/2 ingress，设置 `clientCertificateMode: require`、
`allowInsecure: false`，并阻止绕过 Envoy 的直连。健康 probe 使用 8081，该端口不得暴露为 ingress；`/healthz` 用于存活，`/readyz` 用于就绪。ACA 会在 Relay 确认参与者角色前要求客户端证书，之后 Relay 才按角色选择 BFF 路径。只有 ACA 覆盖输入的 XFCC 并阻止直连绕过时，转发证书才可信。ACA review template 尚未应用。当前 Product API 位于 East Asia internal ACA，而 East Asia 环境没有 VNet；West US 2 的 VNet 属于另一个未连接环境，且 Azure 尚未配置 PostgreSQL Flexible Server。生产前必须评审能够同时访问 Directory 和各 Product owner 服务的私网拓扑。

Relay 只传输 Workspace request。Product、Kernel/Lease、Runtime 和 Artifact authority 仍属于各自组件；host 不加载 fixture handler，也不取得 fixture 状态。

## PostgreSQL 权威存储（独立异步接口）

`PostgresWorkspaceDirectory` 提供成员关系、已分配 Product 角色和已发布 descriptor 的异步读取。`organizations_for_verified_identity` 只接收独立可信 OIDC verifier 提供的 issuer/subject 并返回候选 organization；无候选或多个候选由调用方拒绝。严格便利方法 `organization_for_verified_identity` 在无映射或多 organization 时返回错误。此存储不验证 OIDC token，也不根据身份声明自动创建成员关系。它实现 object-safe async `WorkspaceDirectory` port；Relay 和 Direct await 目录读取，并区分存储错误与非成员。适配器可注入这些服务，不使用 `block_on` 或同步数据库调用。Relay host 使用此异步 port，并要求 `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`；没有文件存储回退。数据库服务、凭据、TLS 信任、网络路径和部署不会由本 crate 或当前 ACA 模板创建。

迁移创建 `memberships`、`roles`、`descriptors` 和只追加的 `audit_events` 表。Operator 授权/撤销与审计行在同一数据库事务中提交；撤销成员时级联删除其角色，并在同一事务记录被移除角色。仅允许分配当前五种 Product 用户命令角色；成员标记和 workload 角色由其他 authority 管理。审计表拒绝 UPDATE/DELETE，reader 角色仅有 SELECT 权限。

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
