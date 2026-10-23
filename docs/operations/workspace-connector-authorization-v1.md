# Workspace connector authorization v1

Workspace Authority issues one short-lived, signed grant for one operation after
it reloads the caller's current Directory membership and
`navigator.connector.operator.v1` role. Exchange requests the grant through its
authenticated mTLS connection; Navigator reaches Exchange through the separate
HMAC-authenticated internal grant facade. Neither request payloads nor Teams
display names can assign roles.
At the Authority boundary, the transport identity is described as the registered connector
authorization workload; deployment configuration binds it to the internal caller by its mTLS
certificate pin and audience.

The Teams identity is the exact configured Microsoft Entra v2 issuer and the
verified activity's immutable `aadObjectId` as a canonical lowercase UUID. The
Directory membership is keyed by this issuer/subject pair, organization and
Workspace. Authority derives the organization and requires the configured
Workspace. Each grant signs the connection, provider, profile, catalog
operation, resource, canonical arguments digest and original request ID. The
grant expires after 15 seconds and is meant for one call. Exchange validates all
bindings and does not reuse grants; role removal applies to the next Authority
lookup.

The closed action catalog is
[`../../contracts/policies/workspace-connector-operator-policy-v1.json`](../../contracts/policies/workspace-connector-operator-policy-v1.json).
Unknown operation IDs and provider mismatches fail closed. Provider CLI actions
use a fixed provider executable and an argument vector; this policy does not
authorize shell command strings.

## Connector-only Authority host

Run `cy-workspace-connector-authority-host` when the deployment needs connector
authorization without the Product execution plane. It serves only
`cyrene.workspace.authority.v2.WorkspaceConnectorAuthorizationService/AuthorizeConnectorOperation` over
mTLS and a loopback `/health` endpoint. It uses the same Directory, Authority
signer, Exchange workload certificate pin, and grant issuer as the full host;
it does not load Product bundles, admission receipts, Device Registry/CRL state,
delegations, web sessions, or invocation outbox state. The existing
`cy-workspace-authority-host` remains the full Product profile.

The connector-only process requires `CYRENE_ORGANIZATION_ID`,
`CYRENE_WORKSPACE_ID`, `CYRENE_AUTHORITY_AAD_TENANT_ID`, and the Directory
reader URL through `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL_FILE` (preferred)
or `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`. It also requires the existing Authority signer and
mTLS file settings, and Exchange workload ID/audience/certificate fingerprint
settings. Its RPC listener defaults to `0.0.0.0:50051`; health defaults to
loopback `127.0.0.1:8080`. It fails to start when the Directory cannot be read
or the Exchange workload pin is missing.

## Add or remove a maintainer

Run these commands through the restricted Directory admin environment. The
operator actor, database URL and audit reason are required; they are never
accepted from a Teams request. Adding a maintainer creates membership if needed
and grants only the connector operator role. Revoking a maintainer removes that
role and leaves the person's membership and other roles intact.

```text
cy-workspace-directory-admin maintainer add \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject d83aeb27-ff11-46d7-b0f7-af8da8d5b8ec \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --reason "Teams connector maintainer"

cy-workspace-directory-admin maintainer revoke \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject d83aeb27-ff11-46d7-b0f7-af8da8d5b8ec \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --reason "Teams connector maintainer access removed"
```

The commands are idempotent and append Directory audit events. `maintainer add`
must use the tenant issuer configured in Authority and the immutable AAD object
ID; a display name, email address or client-provided role is not an identity.

## Workspace connector authorization v1 | Workspace connector 授权 v1

Workspace Authority 每次签发 grant 前都会重新读取当前 Directory membership 与
`navigator.connector.operator.v1` role。grant 只允许一个具体操作，且只有 15 秒有效。
Navigator 通过 HMAC-authenticated Exchange 内部 grant facade 请求授权；Exchange 再通过
mTLS 调用 Authority。Teams 显示名、用户消息字段和请求体中的 role 都不是授权来源。
在 Authority 边界，传输身份统一称为已注册的 connector authorization workload；部署配置通过 mTLS
证书 pin 与 audience 将它绑定到内部调用方。

Teams 用户身份必须是 Authority 配置的 Microsoft Entra v2 issuer，以及经验证活动中的
不可变 `aadObjectId`（canonical 小写 UUID）。Directory membership 以 issuer/subject、组织和
Workspace 为键。Authority 从部署配置读取组织，并要求请求使用该 Authority 配置的
Workspace。签名 grant 绑定连接、provider、profile、封闭目录中的 operation、resource、规范
arguments digest 和原始 request ID。Exchange 必须逐字段校验且每次调用重新申请，不能缓存或
复用 grant；role 撤销会影响下一次 Authority 查询。

封闭 operation catalog 见
[`workspace-connector-operator-policy-v1.json`](../../contracts/policies/workspace-connector-operator-policy-v1.json)。未知 operation 或 provider 不匹配都会拒绝。Provider CLI 只接受固定可执行文件和参数数组；此 policy 不授权 shell 命令字符串。

## Connector-only Authority host | 仅 Connector 授权的 Authority host

当部署只需要 Connector 授权时，运行 `cy-workspace-connector-authority-host`。它只通过
mTLS 提供 `cyrene.workspace.authority.v2.WorkspaceConnectorAuthorizationService/AuthorizeConnectorOperation`，并提供 loopback
`/health`。它复用相同的 Directory、Authority signer、Exchange workload 证书 pin 和 grant 签发逻辑，
不加载 Product bundle、admission receipt、Device Registry/CRL、delegation、web session 或 invocation
outbox。现有 `cy-workspace-authority-host` 仍是完整 Product profile。

最小进程要求 `CYRENE_ORGANIZATION_ID`、`CYRENE_WORKSPACE_ID`、
`CYRENE_AUTHORITY_AAD_TENANT_ID`，以及 Directory reader URL（优先使用
`CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL_FILE`，也可通过 `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL` 提供）。还需现有 Authority signer 与
mTLS 文件，以及 Exchange workload ID/audience/证书指纹配置。RPC 默认监听 `0.0.0.0:50051`；health
默认监听 loopback `127.0.0.1:8080`。Directory 无法读取或缺少 Exchange certificate pin 时，进程拒绝启动。

## 增删维护者

通过受限的 Directory admin 环境运行上述 `maintainer add` 或 `maintainer revoke` 命令。管理员 actor、数据库 URL 与审计原因来自可信服务配置，不从 Teams 请求读取。添加时如无 membership 会先创建 membership，然后仅授予连接器操作 role；撤销时只移除此 role，保留用户 membership 和其他 roles。

命令可安全重复执行，并会追加 Directory audit event。`--issuer` 必须匹配 Authority 配置的 tenant issuer，`--subject` 必须是不可变 AAD object ID；显示名、邮箱或客户端自报 role 都不能代替身份。
