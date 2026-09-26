# Workspace Web BFF v1

This directory defines the same-origin HTTP/JSON contract for a browser-facing
Backend for Frontend (BFF). It is a versioned contract only; it does not claim a
deployed BFF or a working OIDC-to-Workspace runtime.

此目录定义浏览器 BFF 使用的同源 HTTP/JSON 契约。当前仅包含版本化契约，不代表 BFF 已部署，也不代表
OIDC 到 Workspace 的运行链路已可用。

## Files

| File | Responsibility |
|---|---|
| `openapi.yaml` | OpenAPI 3.1.2 routes, request/response JSON, closed operation keys, CSRF, errors, and size limits. |
| `../../tck/workspace-web/v1/scenarios.tsv` | Language-neutral acceptance matrix for the ingress, session, membership, projection, and proxy rules. |

## Authority and dependency

The BFF does not define another Product API registry. Its operation keys are
exactly the `cyrene.workspace.v1.WorkspaceProductApiOperation` enum values.
The matching row in
`contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv` is the
only source for owner, Product OpenAPI `operationId`, and `READ` or `COMMAND`
kind. A deployment must fail closed if the enum and manifest differ, a key is
unknown, or an owner operation has no accepted Product contract.

BFF 不另行定义 Product API registry。operation key 必须与
`cyrene.workspace.v1.WorkspaceProductApiOperation` enum 值完全一致。
`contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv` 中对应行是 owner、Product OpenAPI
`operationId` 及 `READ` 或 `COMMAND` 类型的唯一来源。如果 enum 与 manifest 不一致、key 未知，或 owner operation
没有已接受的 Product 契约，部署必须 fail closed。

The Workspace Fabric request used for a Product call must carry the validated
W3C `traceparent`. The integrated Workspace contract must retain that field in
`WorkspaceApiRequest` when the Product API projection is combined with the
trace propagation change. The BFF does not invent a parallel tracing envelope.

Product 调用使用的 Workspace Fabric request 必须携带通过验证的 W3C `traceparent`。Product API projection 与
trace propagation 改动整合时，Workspace 契约必须在 `WorkspaceApiRequest` 中保留该字段。BFF 不另造 tracing envelope。

## Authentication and ingress trust

The browser sends only the host-only `__Host-cyrene-session` cookie. The
same-origin Nginx ingress routes `/api/workspace/v1/` to a private ACA BFF. The
configured Easy Auth token store supplies its server-side
`X-MS-TOKEN-AAD-ACCESS-TOKEN` value to that private origin. The BFF validates
the signed Azure AD access token itself using the configured issuer's fixed
JWKS, an asymmetric algorithm allowlist, exact configured `iss` and `aud`,
`exp`, `nbf`, and the required delegated scope `Workspace.Web.Access` in `scp`.
The configured API audience and scope must be provisioned before exposure.

Ingress removes caller-supplied `Authorization`, `X-MS-CLIENT-PRINCIPAL`,
`X-MS-TOKEN-*`, `X-Cyrene-Verified-Principal`, and `X-Cyrene-Principal-*`
headers, then forwards only the token-store access token on the private hop.
The BFF never treats `X-MS-CLIENT-PRINCIPAL`, a plain forwarded identity
header, or an unverified token-store header as a principal. It rejects
`alg=none`, symmetric algorithms, unknown key IDs, token-supplied `jku`/`x5u`,
duplicate token headers, invalid signature/claims/scope, or a missing token.
The BFF origin is reachable only from the trusted ingress; public network
paths and direct container/app-origin access are blocked.

After validating the access token, the runtime maps the exact verified issuer
and subject through the server-side Directory to one authorized organization.
The organization is not accepted from a browser header, query, body, or an
unsigned Easy Auth principal header. Missing or ambiguous mappings fail closed.
Only then may the runtime construct an immutable
`VerifiedWebPrincipal { identity: UserIdentityRef, organization_id,
expires_at_unix_ms }`, with the issuer and subject from the verified token and
expiry from its `exp`. The Workspace service's `WorkspaceDirectory::discover`
and `WorkspaceDirectory::is_member` remain the membership authority; the
authenticated Workspace API adapter establishes its own
`WorkspaceCallerContext` after the membership check.

The browser never receives the access token. AAD `token store` and the
`Workspace.Web.Access` API scope are not enabled in the current deployment, so
this contract cannot be exposed until both are configured and their validation
passes the TCK. Existing unsigned `X-MS-CLIENT-PRINCIPAL` data cannot substitute
for this gate.

浏览器只发送 host-only 的 `__Host-cyrene-session` cookie。同源 Nginx ingress 将 `/api/workspace/v1/` 路由到私有 ACA BFF。
已配置的 Easy Auth token store 在服务端向该私有 origin 提供 `X-MS-TOKEN-AAD-ACCESS-TOKEN`。BFF 使用固定 issuer JWKS
及非对称算法 allowlist，自行验证 Azure AD access token 的签名、精确配置的 `iss` 和 `aud`、`exp`、`nbf`，并验证
`scp` 中包含必要的 delegated scope `Workspace.Web.Access`。对外开放前必须先配置目标 API audience 和 scope。

入口删除调用方提供的 `Authorization`、`X-MS-CLIENT-PRINCIPAL`、`X-MS-TOKEN-*`、`X-Cyrene-Verified-Principal` 和
`X-Cyrene-Principal-*` header，然后只在私有 hop 上转发 token-store access token。BFF 不会把 `X-MS-CLIENT-PRINCIPAL`、
普通 forwarded identity header 或未验证的 token-store header 当作 principal。拒绝 `alg=none`、对称算法、未知 key ID、
token 提供的 `jku`/`x5u`、重复 token header、无效签名/claim/scope 或缺失 token。BFF origin 只允许可信入口访问；公网路径和
直接访问容器或应用 origin 的通路都必须阻断。

access token 验证成功后，runtime 使用服务器端 Directory 将精确的已验证 issuer 和 subject 映射到唯一获准的 organization。
organization 不能从浏览器 header、query、body 或未签名 Easy Auth principal header 获取。映射缺失或不唯一时 fail closed。
此后 runtime 才可构造不可变的 `VerifiedWebPrincipal { identity: UserIdentityRef, organization_id,
expires_at_unix_ms }`；issuer 和 subject 来自已验证 token，过期时间来自 `exp`。Workspace service 的
`WorkspaceDirectory::discover` 和 `WorkspaceDirectory::is_member` 仍是 membership authority；已认证的 Workspace API adapter
在 membership 检查成功后建立自己的 `WorkspaceCallerContext`。

浏览器不会收到 access token。当前部署未启用 AAD `token store` 和 `Workspace.Web.Access` API scope，因此完成两项配置并通过 TCK
前不得开放本契约。现有未签名的 `X-MS-CLIENT-PRINCIPAL` data 不能替代该门槛。

## Session and Workspace behavior

`GET /api/workspace/v1/session` returns only verified `issuer`, `subject`,
`organizationId`, and `expiresAt`. It never returns an OIDC token, device
certificate, Workspace relay credential, Product token, or API key. API
requests return RFC 9457 JSON errors and never redirect to an HTML login page.

`GET /api/workspace/v1/workspaces` derives the `DiscoverWorkspacesRequest` identity
from the verified principal. It accepts no caller-supplied issuer, subject, or
organization override. Before each Product call, the BFF performs an
authoritative member-scoped lookup for the requested Workspace using that same
verified identity. A browser-provided Workspace ID is only a selection hint;
it is never proof of membership. The returned summaries omit connection
candidates, connection URIs, routing hints, device enrollment and credentials.

`GET /api/workspace/v1/session` 只返回已验证的 `issuer`、`subject`、`organizationId` 和 `expiresAt`。不返回 OIDC token、
device certificate、Workspace relay credential、Product token 或 API key。API 请求以 RFC 9457 JSON 返回错误，
绝不重定向到 HTML 登录页面。

`GET /api/workspace/v1/workspaces` 根据已验证 principal 派生 `DiscoverWorkspacesRequest` 身份，不接受调用方提供或覆盖
issuer、subject、organization。每次 Product 调用前，BFF 都使用同一已验证身份对所选 Workspace 进行权威的
member-scoped 查询。浏览器提供的 Workspace ID 只是选择提示，不能证明 membership。返回的摘要不含 connection
candidate、connection URI、routing hint、device enrollment 或 credential。

No BFF database, session table, membership cache, Product state store, or
device-credential store is introduced. The identity provider owns login
sessions, Workspace owns membership and Workspace state, and each Product owns
its domain records and command effects.

Every JSON request and response body on these routes, including session,
discovery, Product and error bodies, is capped at 4 MiB. A Product response
above the cap returns `502` without forwarding its body. A member discovery
result that cannot fit returns `503` without returning a partial list. BFF-owned
RFC 9457 errors use the closed schema and field-length limits in the OpenAPI
document.

不新增 BFF database、session table、membership cache、Product state store 或 device-credential store。身份提供方
拥有登录 session，Workspace 拥有 membership 和 Workspace state，每个 Product 拥有自己的领域记录和命令效果。

这些 route 上的所有 JSON 请求和响应 body（包括 session、discovery、Product 和 error）均限制为 4 MiB。Product 响应超限时
返回 `502` 且不转发 body。member discovery 结果无法装入限制时返回 `503`，不返回部分列表。BFF 自身 RFC 9457 error 使用
OpenAPI 文档中的封闭 schema 与字段长度限制。

## Product proxy rules

- Product invocation uses one browser-facing `POST` envelope for both semantic
  `READ` and `COMMAND` operations. The BFF derives owner, Product `operationId`,
  semantic kind, and the upstream HTTP method from the canonical projection
  manifest and the owning Product OpenAPI document. The caller cannot submit a
  Product URL, host, arbitrary path, or method override. This allows a safe
  Product `READ` whose owner operation uses POST and requires JSON input.
- The BFF checks Workspace membership for every call, maps only the opaque
  `workspaceId`, opaque `resourceId`, validated JSON body, and optional
  `Idempotency-Key` to the Workspace Product API request, and does not create
  Product state. `workspaceId` maps only to an owner path parameter named
  `workspace_id` or `workspaceId`, when declared. `resourceId` maps only to the
  single remaining owner path parameter, when declared. The BFF validates each
  identifier against its Product path-parameter schema. It fails closed if the
  mapping is ambiguous, more than one non-Workspace path parameter remains, or
  the owner operation requires an unsupported query/header parameter.
- A JSON request body is required only when the Product operation declares one;
  it is validated against the owner schema and forwarded without edits. The BFF
  does not infer operation kind or upstream method from the browser HTTP verb.
- Request and response JSON bodies are valid UTF-8 and at most 4 MiB each.
  Requests use `Content-Type: application/json`; accepted response types are
  `application/json` and `application/problem+json`. Oversize input returns
  `413`; `Accept` must permit one of the two JSON response types; invalid or
  unsafe upstream response returns `502`.
- The BFF validates only resource-reference fields identified by the
  owning Product OpenAPI schema, then returns the original response bytes. If
  that schema is absent or a declared reference is absolute, has an authority,
  or uses a network-path prefix, the BFF fails closed with `502`.
- The Product HTTP status and JSON bytes are preserved. `Location`,
  `Content-Location`, `Set-Cookie`, `Authorization`, hop-by-hop headers, and
  upstream server-identifying headers are not forwarded. All BFF API responses
  use `Cache-Control: no-store`.
- Product-owned resource references use the relative
  `ProductResourceReference` object: an allowlisted read `operation` plus an
  opaque `resourceId`, resolved under the current Workspace through this BFF.
  It has no `href`, URL, path, host, or credential. Absolute service URLs,
  network-path references, internal Product origins, local paths, device
  details and container identifiers are forbidden. Product bodies remain
  owner-owned; the BFF validates these declared fields against the owner
  OpenAPI schema but does not rewrite JSON. Missing schemas or invalid refs fail
  closed with `502`.
- Product response schemas must not expose a raw Product service URL, path, or
  internal routing address in a resource-link field. If an owner contract has
  not declared these fields using the closed `ProductResourceReference` shape,
  the BFF returns `502` without forwarding the body.
- The BFF accepts an optional valid W3C Trace Context v00 `traceparent`; when
  absent, the trusted runtime creates a new trace context. The context is
  forwarded through Workspace Fabric. Trace fields never affect identity or
  authorization.

- Product invocation 使用一个浏览器侧 `POST` envelope，承载语义上的 `READ` 和 `COMMAND` operation。BFF 从规范 projection
  manifest 与 owner 的 Product OpenAPI 文档派生 owner、Product `operationId`、语义类型和上游 HTTP method。调用方不能提交
  Product URL、host、任意 path 或覆盖 method。这样也可调用安全的 Product `READ`（owner operation 使用 POST 且要求 JSON 输入）。
- 每次调用都检查 Workspace membership；BFF 只把 `workspaceId`、不透明 `resourceId`、通过校验的 JSON body 和可选的
  `Idempotency-Key` 映射到 Workspace Product API request，不创建 Product state。`workspaceId` 只映射到 owner path 参数
  `workspace_id` 或 `workspaceId`。`resourceId` 只映射到唯一剩余的 owner path 参数。BFF 按 Product path-parameter schema
  校验标识。若映射有歧义、存在多个非 Workspace path parameter，或 owner operation 需要不支持的 query/header parameter，必须 fail closed。
- 仅当 Product operation 声明 request body 时才要求 JSON body；BFF 按 owner schema 校验并原样转发。BFF 不从浏览器 HTTP verb
  推导 operation kind 或上游 method。
- 请求及响应 JSON body 均须为合法 UTF-8，且各自不超过 4 MiB。请求使用 `Content-Type: application/json`；
  接受的响应类型为 `application/json` 和 `application/problem+json`，`Accept` 必须允许其中至少一种。超限输入返回 `413`，无效或不安全的上游响应返回 `502`。
- Product HTTP status 和 JSON bytes 原样保留。不得转发 `Location`、`Content-Location`、`Set-Cookie`、
  `Authorization`、hop-by-hop header 和可识别上游 server 的 header。所有 BFF API 响应使用 `Cache-Control: no-store`。
- Product 所有的资源引用使用 relative `ProductResourceReference` object：一个 allowlisted read `operation` 加不透明
  `resourceId`，在当前 Workspace 下通过本 BFF 解析。该 object 不包含 `href`、URL、path、host 或 credential。禁止绝对
  service URL、network-path reference、内部 Product origin、本地路径、device 详情和 container identifier。Product body
  仍归 owner 所有；BFF 按 owner OpenAPI schema 校验这些声明字段，但不重写 JSON。缺少 schema 或引用无效时 fail closed 并返回 `502`。
- Product response schema 不得在 resource-link field 中暴露原始 Product service URL、path 或内部 routing address。Owner contract
  未用封闭 `ProductResourceReference` shape 声明这些字段时，BFF 返回 `502` 且不转发 body。
- BFF 可接受合法的 W3C Trace Context v00 `traceparent`；未提供时由可信 runtime 创建新的 trace context。该 context
  通过 Workspace Fabric 传递。Trace 字段不得影响身份或授权。

## CSRF and error behavior

The OIDC ingress sets `__Host-cyrene-session` as `Secure`, `HttpOnly`,
`SameSite=Lax`, `Path=/`, without `Domain`. After validating the access-token session,
`GET /api/workspace/v1/session` returns a signed CSRF token in its JSON body and sets it in an
HttpOnly `__Secure-cyrene-csrf` cookie with `Secure`, `SameSite=Strict`,
`Path=/api/workspace/v1`, and no `Domain`. The token is MAC-bound to the verified issuer,
subject, organization, access-token session, and an expiration no later than the verified principal.
The CSRF expiry equals the verified principal expiry, so concurrent session refreshes for the
same principal and access-token session return the same JSON token and cookie value.
The token contains neither the access token nor its fingerprint. The BFF has no server-side CSRF
session store; its MAC key is injected by the trusted runtime.

Every Product invocation, including semantic `READ`, requires `Origin` to exactly match
the configured Client origin. When the canonical projection manifest marks an operation
`COMMAND`, the BFF also requires `X-CSRF-Token` to byte-match the HttpOnly cookie and
verify against the current principal and access-token session before contacting Workspace.
Semantic `READ` operations use the same POST envelope and exact Origin check, but do not
require a CSRF header or cookie.

BFF-owned errors use `application/problem+json` (RFC 9457), with `status`
equal to the HTTP status and a stable `code`. Product errors keep the owning
Product status and JSON response body. Error details never disclose access-token
contents or claims, signing keys, device credentials, relay routing data, internal
service addresses or upstream URLs.

OIDC 入口设置 `__Host-cyrene-session` cookie，属性为 `Secure`、`HttpOnly`、`SameSite=Lax`、`Path=/` 且不设置
`Domain`。BFF 验证 access-token session 后，`GET /api/workspace/v1/session` 在 JSON body 返回签名 CSRF token，
并将其写入 HttpOnly `__Secure-cyrene-csrf` cookie；属性为 `Secure`、`SameSite=Strict`、
`Path=/api/workspace/v1` 且不设置 `Domain`。MAC 将 token 绑定 verified issuer、subject、organization、
access-token session 与不晚于 principal 过期时间的 expiry。token 不含 access token 或其 fingerprint；MAC key 由可信 runtime 注入，
不新增 CSRF server-side session store。
CSRF expiry 与 verified principal expiry 相同，因此同一 principal 和 access-token session 的并发 session refresh 会返回相同的 JSON token 与 cookie 值。

每次 Product invocation（包括语义类型 `READ`）均要求 `Origin` 精确匹配配置的 Client origin。只有 closed
projection manifest 中类型为 `COMMAND` 的 operation 还要求 `X-CSRF-Token` 与 HttpOnly cookie 字节值完全一致，并通过
当前 principal 与 access-token session 的 MAC 校验；缺少/不匹配 token 或跨域请求时，BFF 必须在访问 Workspace 前拒绝。
语义类型为 `READ` 的 operation 使用相同 POST invocation envelope 与 exact Origin 检查，但不要求 CSRF header 或 cookie。

BFF 自身错误使用 `application/problem+json`（RFC 9457），`status` 必须与 HTTP status 一致，并提供稳定的 `code`。
Product 错误保留 owner 的 status 和 JSON response body。错误详情不得泄漏 access token 或 claim、签名密钥、device credential、
relay routing 数据、内部 service 地址或上游 URL。

## Deployment status

The current Client deployment is a static Nginx site. Its Nginx configuration
currently routes Product prefixes directly and sends generic `/api/` traffic
to Exchange. The intended same-origin rule is a more-specific
`location /api/workspace/v1/` to the private ACA BFF, but that route is not
deployed today and the prefix would fall through to the generic route.
Existing anonymous `/api` gating by ACA Easy Auth does not implement these
routes, provide the required token-store access token/API scope, establish the
Directory mapping, or connect to Workspace Fabric. The Exchange session/auth
stub and Navigator local pairing are not this BFF.

Before exposure, a coordinated ingress change must install the more-specific
same-origin `location /api/workspace/v1/` ahead of the generic `/api/` route,
enable the Easy Auth token store and `Workspace.Web.Access` API scope, block
direct access to the BFF origin, and prevent browser calls from bypassing the
BFF to Products. The BFF must validate the signed access token and perform the
server-side Directory mapping; it must never derive `VerifiedWebPrincipal`
directly from `X-MS-CLIENT-PRINCIPAL`. Until token/scope configuration, trusted
ingress boundary, private BFF origin, Directory and Workspace adapter, and
Product projection runtime pass this contract's TCK, `/api/workspace/v1` must
remain disabled and fail closed. This contract does not change Client routes or
assert runtime completion.

当前 Client 部署是静态 Nginx 站点。其 Nginx 配置当前将 Product 前缀直接转发，并把通用 `/api/` 流量转发至 Exchange。
目标同源规则是更具体的 `location /api/workspace/v1/`，转发至私有 ACA BFF；当前尚未部署该路由，因此该 prefix 会落入通用 route。
现有 ACA Easy Auth 对匿名 `/api` 的门禁没有实现这些 route，也没有提供所需 token-store access token/API scope、建立 Directory
映射或连接 Workspace Fabric。Exchange session/auth stub 和 Navigator 本地 pairing 都不是本 BFF。

对外开放前，必须通过协调后的 ingress 改动，在通用 `/api/` route 前安装同源 `location /api/workspace/v1/`，启用 Easy Auth
token store 与 `Workspace.Web.Access` API scope，阻断对 BFF origin 的直接访问，并阻止浏览器绕过 BFF 直接调用 Product。BFF 必须
验证签名 access token 并执行服务端 Directory 映射；不得直接从 `X-MS-CLIENT-PRINCIPAL` 派生 `VerifiedWebPrincipal`。
token/scope 配置、可信入口边界、私有 BFF origin、Directory 与 Workspace adapter 以及 Product projection runtime 通过本契约 TCK
前，必须禁用 `/api/workspace/v1` 并 fail closed。本契约不修改 Client route，也不宣称 runtime 已完成。
