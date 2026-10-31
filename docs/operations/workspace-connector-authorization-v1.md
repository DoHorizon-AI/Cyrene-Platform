# Workspace connector authorization v1

Workspace Authority issues one short-lived, signed grant for one operation after
it reloads the caller's current Directory membership and checks the role allowed
for that operation. `navigator.connector.operator.v1` retains its existing
catalog behavior. `navigator.automation.viewer.v1` can request only the six
Microsoft Graph read operations listed in the policy; it does not grant Graph
writes, other providers, vendor CLI, computer access, or credential export.
Pilot members receive this viewer role by default. Both automation roles can
also use the existing `msteams.message.reply` transport operation so a member
can receive a response to their own authorized Work task. This does not permit
arbitrary Teams messages or add any source operation. The owner may later grant
`navigator.office.task_writer.v1` explicitly; it is limited to the 17 Planner,
To Do, and selected SharePoint metadata operations in policy. Calendar operations
remain limited to the existing connector operator role. Neither role grants OAuth
scopes or access to other providers.
Exchange requests the grant through its authenticated mTLS connection; Navigator
reaches Exchange through the separate HMAC-authenticated internal grant facade.
Neither request payloads nor Teams display names can assign roles.
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

The automation transport catalog contains only `msteams.message.reply` for
provider `msteams`. Each reply grant is signed for the original actor, exact
connection/profile, target and canonical arguments; the viewer's six Graph
reads and the task writer's 17 Planner, To Do, and selected SharePoint metadata
operations remain separate role sets. Calendar operations are outside both sets
and require the existing connector operator role. A grant signature is not proof that the actor owns the target
conversation. Work must bind the task to the verified Teams actor, and Exchange
and the Teams callback must enforce that binding before sending. This policy
does not authorize sending a new message to an arbitrary user or conversation.

The closed action catalog is
[`../../contracts/policies/workspace-connector-operator-policy-v1.json`](../../contracts/policies/workspace-connector-operator-policy-v1.json).
Unknown operation IDs and provider mismatches fail closed. The viewer's exact
Graph read set is `microsoft_graph.planner.plan.get`,
`microsoft_graph.planner.tasks.list`, `microsoft_graph.planner.task.get`,
`microsoft_graph.sharepoint.site.get`, `microsoft_graph.sharepoint.list.get`,
and `microsoft_graph.sharepoint.list.items.delta`. The task writer's 17-operation
set is those six reads plus `microsoft_graph.planner.plans.list`, Planner task
create/update, all six To Do list/task read and write operations, and the two
selected SharePoint drive metadata reads (`microsoft_graph.sharepoint.drives.list`
and `microsoft_graph.sharepoint.drive.items.list`). Calendar list, event, and
free/busy operations are supported only for the existing connector operator role;
task writers do not inherit Calendar access. `azure_devops.observations.poll` is in the canonical operation
catalog but is denied to viewers until the provider pins and enforces the
original caller for its source profile.

Directory role membership is not proof that a user can read an arbitrary
provider source or target. Authority binds each signed grant to the exact actor,
Workspace, connection, provider, profile, operation, resource, argument digest,
and request ID, but it does not query provider profile ownership or source ACLs.
The Graph provider path must use the requesting actor's Account Center lease and
validate `/me` with that same delegated token before reading or writing. Planner
and To Do writes require that actor's explicit `Tasks.ReadWrite` consent, while
Calendar event writes require `Calendars.ReadWrite`; Directory roles do not grant
OAuth scopes. This Authority change does
not implement or observe those provider checks. Until they are deployed and
verified, keep Graph sources unavailable to pilot members. If a same-actor
profile lease, source authorization check, or required OAuth scope is missing,
the provider must reject the call; it must not use an owner's shared credential
or treat a Directory role as source access. ADO viewer reads remain disabled
pending an equivalent caller binding. The viewer and task writer roles grant no
existing cloud service-identity or computer read path.

Provider CLI actions use a fixed provider executable and an argument vector;
this policy does not authorize shell command strings. GitHub and Hugging Face
CLI calls use the provider-bound `github.vendor.cli.execute` and
`huggingface.vendor.cli.execute` operation IDs; cloud profiles use
`cloud.vendor.cli.execute`. These remain outside the viewer role.

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
accepted from a Teams request. The fixed `maintainer add` and `maintainer
revoke` commands continue to manage only the connector operator role. Use the
generic role commands for the automation viewer and Office task writer roles;
granting or revoking one role leaves membership and other roles intact.

```text
cy-workspace-directory-admin maintainer add \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --reason "Teams connector maintainer"

cy-workspace-directory-admin maintainer revoke \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --reason "Teams connector maintainer access removed"

cy-workspace-directory-admin grant-membership \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --role navigator.automation.viewer.v1 \
  --reason "automation pilot viewer"

cy-workspace-directory-admin grant-role \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --role navigator.automation.viewer.v1 \
  --reason "automation pilot viewer"

cy-workspace-directory-admin grant-role \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --role navigator.office.task_writer.v1 \
  --reason "Office task writer approved"

cy-workspace-directory-admin revoke-role \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --role navigator.automation.viewer.v1 \
  --reason "automation pilot viewer access removed"

cy-workspace-directory-admin revoke-role \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id> \
  --role navigator.office.task_writer.v1 \
  --reason "Office task writer access removed"

cy-workspace-directory-admin membership get \
  --issuer https://login.microsoftonline.com/<tenant-id>/v2.0 \
  --subject 11111111-1111-4111-8111-111111111111 \
  --organization <organization-id> \
  --workspace <workspace-id>
```

The commands are idempotent and append Directory audit events. `maintainer add`
must use the tenant issuer configured in Authority and the immutable AAD object
ID; a display name, email address or client-provided role is not an identity.
Use `grant-membership` when adding a new pilot member; pass only
`navigator.automation.viewer.v1` to this scoped helper. Use `grant-role` when the
membership already exists. Grant `navigator.office.task_writer.v1` only after
owner approval and the same actor's Account Center `Tasks.ReadWrite` consent;
the Directory command does not grant OAuth consent.
`membership get` uses the configured read-only Directory URL and emits one JSON
object containing the exact requested scope, `isMember`, and the explicit role
array. An absent membership returns `isMember: false` and `roles: []`; a present
membership with no assigned roles returns `isMember: true` and `roles: []`.

## Workspace connector authorization v1 | Workspace connector 授权 v1

Workspace Authority 每次签发 grant 前都会重新读取当前 Directory membership，并按操作检查 role。
`navigator.connector.operator.v1` 保留原有操作目录；新 pilot 成员默认只授予 `navigator.automation.viewer.v1`，仅可请求 policy 中列出的六个 Graph 只读操作。两个 automation role 还可使用既有 `msteams.message.reply` 传输操作，让成员收到自己已授权 Work task 的回复；这不允许任意 Teams 消息，也不会增加 source operation。owner 可稍后显式授予 `navigator.office.task_writer.v1`，该角色只允许 policy 中的 17 个 Planner、To Do 和选定 SharePoint 元数据 operation；Calendar operation 仍仅限既有 connector operator role。两种角色都不会授予 OAuth scope 或其他 provider access。
grant 只允许一个具体操作，且只有 15 秒有效。
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

Automation transport catalog 只包含 provider `msteams` 的 `msteams.message.reply`。每个 reply grant 都绑定原始 actor、精确 connection/profile、target 和规范 arguments；viewer 的六个 Graph read 与 task writer 的 17 个 Planner、To Do 和选定 SharePoint 元数据 operation 仍是彼此独立的 role set。Calendar operation 不属于这两个集合，只允许既有 connector operator role。grant signature 不代表 actor 拥有该会话；Work 必须把任务绑定到已验证的 Teams actor，Exchange 与 Teams callback 也必须在发送前执行该绑定。此策略不允许向任意用户或会话发送新消息。

封闭 operation catalog 见
[`workspace-connector-operator-policy-v1.json`](../../contracts/policies/workspace-connector-operator-policy-v1.json)。viewer 的 Graph 只读集合仍是 `microsoft_graph.planner.plan.get`、`microsoft_graph.planner.tasks.list`、`microsoft_graph.planner.task.get`、`microsoft_graph.sharepoint.site.get`、`microsoft_graph.sharepoint.list.get` 和 `microsoft_graph.sharepoint.list.items.delta`。`navigator.office.task_writer.v1` 的 17 项集合是这六个读取 operation，加上 `microsoft_graph.planner.plans.list`、Planner task create/update、六个 To Do list/task read/write operation，以及 `microsoft_graph.sharepoint.drives.list` 和 `microsoft_graph.sharepoint.drive.items.list` 两个选定 drive metadata read。Calendar list、event 与 free/busy operation 只允许既有 connector operator role；viewer 和 task writer 均不可请求。`azure_devops.observations.poll` 已进入 canonical operation catalog，但在 provider 绑定并强制校验原始调用者的 source profile 前，viewer 请求仍会拒绝。

Directory role membership 不代表用户可读取任意 provider source 或 target。Authority 会把 grant 精确绑定到 actor、Workspace、connection、provider、profile、operation、resource、arguments digest 与 request ID，但不会查询 provider profile owner 或 source ACL。Graph provider 必须使用请求 actor 自己的 Account Center lease，并以同一个 delegated token 校验 `/me` 后才读取或写入；Planner 和 To Do write 还要求该 actor 显式同意 `Tasks.ReadWrite`，Calendar event write 要求 `Calendars.ReadWrite`，Directory role 本身不授予 OAuth scope。此 Authority 改动不实现或观测这些 provider 检查。在这些检查部署并验证前，pilot viewer 与 task writer 均不得访问 Graph source。缺少 same-actor profile lease、source 授权检查或所需 OAuth scope 时，provider 必须拒绝调用，不能使用 owner 共享 credential，也不能把 Directory role 当作 source access。ADO viewer 读取在有等效调用者绑定前保持关闭。viewer 与 task writer role 不会开放既有 cloud service-identity 或 computer 读取路径。

Provider CLI 使用固定 executable 和 argument vector；policy 不授权 shell 命令字符串。GitHub、Hugging Face 和 cloud 的 vendor CLI operation 均不属于 viewer role。

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

通过受限的 Directory admin 环境运行这些命令。管理员 actor、数据库 URL 与审计原因来自可信服务配置，不从 Teams 请求读取。固定的 `maintainer add` 与 `maintainer revoke` 仍只管理 connector operator role。使用通用 `grant-role` 和 `revoke-role` 管理 automation viewer 与 Office task writer；撤销一个 role 只移除该 role，保留用户 membership 和其他 roles。

命令可安全重复执行，并会追加 Directory audit event。`--issuer` 必须匹配 Authority 配置的 tenant issuer，`--subject` 必须是不可变 AAD object ID；显示名、邮箱或客户端自报 role 都不能代替身份。`membership get` 使用配置的只读 Directory URL，输出包含请求 scope、`isMember` 与明确的 role 数组的单个 JSON object。membership 不存在时返回 `isMember: false`、`roles: []`；membership 存在但没有显式 role 时返回 `isMember: true`、`roles: []`。
新 pilot 用户通过 `grant-membership` 只获得 viewer role。仅在 owner 显式批准且同一用户完成 Account Center `Tasks.ReadWrite` consent 后，才用 `grant-role` 授予 task writer；该 Directory 命令不会授予 OAuth consent。
