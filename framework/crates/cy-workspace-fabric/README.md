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
| `src/relay.rs` | Live application request routing without Workspace authority. | 不拥有 Workspace 权威的实时应用请求路由。 |
| `src/transport.rs` | Direct candidate selection and outbound mTLS Relay transport. | 直连候选选择与出站 mTLS Relay 传输。 |
| `src/bin/` | Fail-closed Relay host and acceptance-only connector/reference frontend fixtures. | fail-closed Relay host 与仅用于验收的 Connector/参考 frontend fixture。 |
| `src/sidecar.rs` | Loopback-authenticated bridge for non-Rust Workspace clients. | 为非 Rust Workspace client 提供 loopback 认证代理。 |
| `src/bin/` | Acceptance relay/connector/frontend fixtures and the loopback sidecar executable. | 验收 Relay/Connector/frontend fixture 与 loopback sidecar 可执行程序。 |

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
certificates, verify TLS peers, or issue credentials. No private keys,
certificate bodies, or session credentials are stored, and this crate does not
itself run a Directory service.

## Relay host security state

`cy-workspace-relay-host` is a runnable, non-fixture process shell around
`WorkspaceRelay`. The production device IAM verifier and directory
administration integration are not implemented, so the host uses an
unavailable verifier that rejects every Relay session and keeps `/readyz` at
HTTP 503. `/healthz` reports only that the process is alive. This is a
fail-closed host scaffold, not a production Relay deployment.

Run the host locally with an owner-only state directory:

```bash
CYRENE_WORKSPACE_RELAY_DIRECTORY=/path/to/private/workspace-relay-state \
  cargo run --locked -p cy-workspace-fabric --bin cy-workspace-relay-host
```

The default listeners are `127.0.0.1:8080` for gRPC and `127.0.0.1:8081` for
health probes. `GET /healthz` returns HTTP 200; `GET /readyz` returns HTTP 503
until production identity and directory administration exist.

The host defaults to loopback. A non-loopback Relay bind requires the explicit
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`
setting. A non-loopback health bind also requires
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`. Both
settings are operator assertions, not proof of live ACA configuration. ACA
must bind both listeners to the container interface (for example
`0.0.0.0:8080` and `0.0.0.0:8081`), explicitly set ingress `targetPort: 8080`
and `transport: http2`, set `allowInsecure: false`, require client
certificates with `clientCertificateMode: require`, and leave `8081` out of
`additionalPortMappings`. Configure startup/liveness probes at `8081/healthz`
and readiness at `8081/readyz`. ACA's default probes target the ingress port,
so explicit probe configuration is required with both listeners. The current
`/readyz` response is 503 by design, which keeps the revision unready and
prevents it receiving ingress traffic. Mount a private persistent directory
and maintain a single active owner for its lock. The host does not read or trust
`X-Forwarded-Client-Cert`; production IAM must validate the enrolled
certificate fingerprint and bind it to an authorized device and Workspace
session before readiness can be enabled. Do not deploy fixture credentials or
`DevelopmentSessionVerifier` as production identity.

The Relay host transports Workspace requests only. Product, Kernel/Lease,
Runtime, and Artifact authorities remain in their owning components; the host
does not load fixture handlers or acquire their state.

## PostgreSQL authority (standalone async port)

`PostgresWorkspaceDirectory` provides async reads of memberships, assigned
Product roles, and published descriptors. `organization_for_verified_identity`
accepts only an issuer/subject pair supplied by a separate trusted OIDC
verifier; it rejects both no mapping and mappings to multiple organizations.
This store does not validate OIDC tokens or create memberships from identity
claims. It deliberately does not implement the crate's current synchronous
`WorkspaceDirectory` trait, so the existing Relay and Direct request paths still
use their configured in-memory or private JSON store until a separate async
caller integration is completed. There is no fallback from PostgreSQL errors
to a snapshot.

The migration creates `memberships`, `roles`, `descriptors`, and append-only
`audit_events` tables. Operator grant/revoke operations write their changes and
audit rows in one database transaction. Revoking a membership cascades its
roles and records the removed roles in that transaction. Only the five current
Product user-command roles can be assigned; membership and workload roles come
from separate authorities. The audit table rejects UPDATE and DELETE, and the
reader role has SELECT-only access.

Configure three separate trusted process URLs:

* `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`: runtime reader login, a member of
  `cyrene_workspace_directory_reader` only.
* `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL`: local provisioning CLI
  login, a member of `cyrene_workspace_directory_operator`.
* `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`: deployment migration
  login with the database privileges needed to create roles and schema.

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

The down migration drops the Directory schema and its data. No PostgreSQL
deployment, credentials, OIDC verifier, or async request-path integration is
included here, so this is a persistence adapter, not a production cutover.

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

存储层只校验指纹格式，不解析证书、不验证 TLS 对端、不签发凭据。这里不保存私钥、证书正文或会话凭据，crate 也不自行运行 Directory 服务。

## Relay host 安全状态

`cy-workspace-relay-host` 是围绕 `WorkspaceRelay` 的可运行非 fixture 进程入口。生产设备 IAM
verifier 与目录管理集成尚未实现，因此 host 使用不可用 verifier，拒绝所有 Relay session，并使
`/readyz` 保持 HTTP 503。`/healthz` 只表示进程存活。这是 fail-closed host 脚手架，不是生产
Relay 部署。

使用仅限服务所有者访问的状态目录，可在本地启动 host：

```bash
CYRENE_WORKSPACE_RELAY_DIRECTORY=/path/to/private/workspace-relay-state \
  cargo run --locked -p cy-workspace-fabric --bin cy-workspace-relay-host
```

默认 gRPC listener 为 `127.0.0.1:8080`，健康探针 listener 为
`127.0.0.1:8081`。`GET /healthz` 返回 HTTP 200；在生产身份和目录管理接通前，
`GET /readyz` 返回 HTTP 503。

host 默认绑定 loopback。Relay 使用非 loopback bind 时，必须显式设置
`CYRENE_WORKSPACE_RELAY_ACA_INGRESS_ASSERTION=client-certificate-required`。该设置只是运维声明，
不能证明 ACA 的实时配置。健康 listener 使用非 loopback bind 时，也必须设置
`CYRENE_WORKSPACE_RELAY_HEALTH_BIND_ASSERTION=probe-only-not-ingress`。ACA 必须在 Envoy 终止客户端
TLS、强制客户端证书；两个 listener 绑定容器网卡（例如 `0.0.0.0:8080` 与
`0.0.0.0:8081`），显式将 ingress `targetPort: 8080`、`transport: http2`、
`allowInsecure: false`，并设置 `clientCertificateMode: require`。`8081` 不得放入
`additionalPortMappings`。启动/存活 probe 配为 `8081/healthz`，就绪 probe 配为
`8081/readyz`。两个 listener 并存时，ACA 默认 probe 会检查 ingress port，因此必须显式配置 probe。
当前 `/readyz` 按设计返回 503，使 revision 保持未就绪并且不接收 ingress 流量。挂载私有持久目录，
并保证同一时间只有一个 owner 持有目录锁。host 不读取或信任
`X-Forwarded-Client-Cert`；生产 IAM 必须验证已登记证书指纹，并将其绑定到授权设备及 Workspace
session 后才能启用 readiness。不得将 fixture credential 或 `DevelopmentSessionVerifier` 用作生产身份。

Relay host 只传输 Workspace request。Product、Kernel/Lease、Runtime 与 Artifact authority 仍属于
各自的组件；host 不加载 fixture handler，也不获得 fixture 状态。

## PostgreSQL 权威存储（独立异步接口）

`PostgresWorkspaceDirectory` 提供成员关系、已分配 Product 角色和已发布 descriptor 的异步读取。`organization_for_verified_identity` 只接收独立可信 OIDC verifier 提供的 issuer/subject；没有映射或映射到多个 organization 时都会拒绝。此存储不验证 OIDC token，也不根据身份声明自动创建成员关系。它暂不实现当前同步的 `WorkspaceDirectory` trait；Relay 和 Direct 请求路径在后续异步接线完成前仍使用已配置的内存或私有 JSON 存储。PostgreSQL 查询失败时不会回退到快照。

迁移创建 `memberships`、`roles`、`descriptors` 和只追加的 `audit_events` 表。Operator 授权/撤销与审计行在同一数据库事务中提交；撤销成员时级联删除其角色，并在同一事务记录被移除角色。仅允许分配当前五种 Product 用户命令角色；成员标记和 workload 角色由其他 authority 管理。审计表拒绝 UPDATE/DELETE，reader 角色仅有 SELECT 权限。

使用三个独立的可信运行时 URL：

* `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`：服务 reader 登录，只加入 `cyrene_workspace_directory_reader`。
* `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL`：本地 provisioning CLI 登录，只加入 `cyrene_workspace_directory_operator`。
* `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`：部署迁移登录，需有创建角色和 schema 的数据库权限。

所有连接均强制 PostgreSQL TLS 证书与主机名校验（`sslmode=verify-full`）；私有数据库 CA 需配置 `sslrootcert`。`CYRENE_WORKSPACE_DIRECTORY_OPERATOR_ID` 从可信进程配置提供审计 actor，CLI 参数不接受 actor ID。服务进程不得使用 operator 或 migration URL。数据库用户、角色成员授权、secret、TLS、备份和部署由 crate 外部管理。

本地 provisioning 示例：

```bash
cy-workspace-directory-admin migrate
cy-workspace-directory-admin grant-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --role workspace.product.command.catalyst.create_dataset.v1 --reason TICKET
cy-workspace-directory-admin revoke-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --reason TICKET
```

down migration 会删除 Directory schema 及其中数据。此改动不包含 PostgreSQL 部署、凭据、OIDC verifier 或异步请求路径接线，因此是持久化 adapter，尚未完成生产切换。

运行：

```bash
cargo test --locked -p cy-workspace-fabric
cargo clippy --locked -p cy-workspace-fabric --all-targets -- -D warnings
```
