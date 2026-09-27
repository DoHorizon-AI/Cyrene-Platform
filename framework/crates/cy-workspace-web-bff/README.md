# Workspace Web BFF application

This crate provides the versioned Workspace Web BFF router and a production-host
composition path. The executable starts only after its required configuration,
PostgreSQL Directory, and pinned Product contract bundle load successfully. Its
readiness probe remains closed until AAD and Relay external trust paths can be
verified without a synthetic user.

本 crate 提供版本化 Workspace Web BFF router 与 production-host 装配路径。只有配置、PostgreSQL Directory 和锁定来源的
Product 合同 bundle 全部加载成功后 executable 才会启动。由于不能用合成用户验证 AAD 与 Relay 外部信任路径，就绪探针仍保持关闭。

## Composition boundary

WebBffState::new requires all providers and a canonical operation catalog:

- WebPrincipalVerifier validates one access token using the fixed issuer,
  audience, JWKS keys, lifetime, and scope rules. Only its immutable
  VerifiedWebPrincipal getters are consumed. The router never creates a
  principal from X-MS-CLIENT-PRINCIPAL or another unsigned identity header.
- WorkspaceDirectory supplies current member-scoped Workspace descriptors.
  The router projects only workspaceId, organizationId, and displayName.
- `FabricWorkspaceProductGateway` receives a verified principal plus typed
  `WorkspaceApiRequest`. It re-discovers descriptors for the principal's
  verified organization, rejects malformed, cross-organization, or duplicate
  Workspace descriptors, and selects only the exact requested Workspace ID.
  It then rechecks membership and Directory roles through
  `WorkspaceCallerContext::from_verified_web_member` before dispatching to
  `WorkspaceApi::handle_authenticated`. Actor, organization, Workspace scope,
  and roles are never accepted from the HTTP request. The legacy unauthenticated
  `WorkspaceApi::handle` is not an acceptable adapter.
- `WorkspaceApiResolver` receives the same verified principal as the API
  request and the selected Directory descriptor. The production host creates a
  fresh `FrontendRelayClient` for that principal, discovers through that signed
  session, requires the full descriptor to match exactly, and binds one exact
  configured Relay candidate. The transport is never cached across users.
- The request path is Web BFF → Frontend Relay session → Workspace Connector →
  Product adapter. Product endpoint manifests are consumed by the Connector,
  not by the BFF. The BFF loads owner OpenAPI schemas to validate browser
  envelopes and responses; it never proxies browser-selected Product URLs.
- ProductOperationCatalog must contain exactly one matching owner OpenAPI
  operation for each canonical projection row. Its operation key, owner,
  Product operationId, and semantic kind must match the TCK. The BFF does not
  maintain a second owner or permission table. Navigator append is retained as
  a canonical provenance row but is deny-only: its request/response schemas are
  not compiled into the callable catalog and no upstream dispatch is allowed.
  For operations 01–12, the loader also requires the owner-specific private
  path namespace and one exact HTTP bearer security scheme:
  `WorkspaceServiceBearer` for Catalyst, Yield, Reactor, and Echo;
  `WorkspaceControlBearer` for Exchange; `NavigatorProductBearer` for operation
  11; and `NavigatorWorkspaceBearer` for operation 12. Missing, alternate, or
  inherited-only security fails startup.

build.rs reads
contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv.
The generated rows are checked against the compiled
WorkspaceProductApiOperation enum at router composition and in crate tests.
The runtime composition root loads the read-only release bundle named by
`CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT`. It verifies the canonical
TCK digest, six repository pins, every raw file hash, and the complete local
`$ref` closure before compiling request, path, and response schemas. The bundle
must contain all 13 TCK operation mappings. Operations 01–12 compile closed
owner request/path/response schemas. Operation 13 must resolve to its recorded
Navigator POST operation but stays deny-only; its open append body is never
compiled as callable input. A missing, stale, partial, remote, or out-of-root
contract prevents readiness.

WebBffState::new 必须接收全部 provider 与 canonical operation catalog：

- WebPrincipalVerifier 使用固定 issuer、audience、JWKS key、token lifetime 与 scope rule 校验 access token；
  router 只消费其不可变 VerifiedWebPrincipal getter，绝不从 X-MS-CLIENT-PRINCIPAL 或其他未签名身份 header
  构造 principal。
- WorkspaceDirectory 提供当前 member-scoped Workspace descriptor；router 只投影
  workspaceId、organizationId 和 displayName。
- `FabricWorkspaceProductGateway` 接收 verified principal 与 typed `WorkspaceApiRequest`。它按 principal 的已验证
  organization 重新发现 descriptors，拒绝格式错误、跨组织或重复的 Workspace descriptor，并且只选择与请求 Workspace ID
  完全相同的项。之后它通过 `WorkspaceCallerContext::from_verified_web_member` 重新查询 membership 和 Directory roles，才调用
  `WorkspaceApi::handle_authenticated`。HTTP request 不能提供 actor、organization、Workspace scope 或 roles。legacy 未认证的
  `WorkspaceApi::handle` 不能作为 adapter。
- `WorkspaceApiResolver` 接收与 API request 相同的 verified principal 和选中的 Directory descriptor。生产 host 为该 principal
  创建新的 `FrontendRelayClient`，通过签名 session discovery，并要求完整 descriptor 精确匹配，再绑定到一个精确配置的 Relay
  candidate。transport 不会跨用户缓存。
- 调用链为 Web BFF → Frontend Relay session → Workspace Connector → Product adapter。Product endpoint manifest 由 Connector
  使用，不由 BFF 消费。BFF 加载 owner OpenAPI schema 以校验浏览器 envelope 与 response；不会代理浏览器选择的 Product URL。
- ProductOperationCatalog 对 canonical projection row 的每个 operation 必须恰有一个匹配的 owner OpenAPI operation。
  operation key、owner、Product operationId 和语义类型必须与 TCK 一致。BFF 不维护第二份 owner 或 permission table。

build.rs 从 contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv 读取 projection rows，并在 router
composition 与 crate tests 中和生成的 WorkspaceProductApiOperation enum 对齐。Runtime composition root 从环境变量
`CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT` 指定的只读 release bundle 加载合同，校验 canonical TCK digest、六个仓库 pin、
每个文件的原始字节 hash 和完整本地 `$ref` 闭包，再编译 request、path、response schema。bundle 必须包含全部 13 个 TCK operation mapping。
Operation 01–12 编译封闭 owner request/path/response schema。Operation 13 必须能解析到所记录的 Navigator POST operation，但保持 deny-only；开放 append body 不会编译成可调用输入。合同缺失、过期、部分、远端或越界均阻止 readiness。

## HTTP and security behavior

- The only routes are GET /api/workspace/v1/session,
  GET /api/workspace/v1/workspaces, and
  POST /api/workspace/v1/workspaces/{workspaceId}/products/{operation}.
- Each request accepts exactly one server-injected
  `Authorization: Bearer <access-token>` header. Duplicate or malformed
  Authorization values and any `X-MS-TOKEN-*` header are rejected. Ingress
  must discard caller-supplied Authorization, rebuild it from the Easy Auth
  token-store value, strip Easy Auth and Cyrene identity headers, and block all
  public access that bypasses that ingress. The access token is passed only to
  the verifier and session-bound CSRF signer; it is never serialized or logged.
- The router requires one exact configured HTTPS Client origin for every
  Product POST, including semantic READ operations. A COMMAND additionally
  requires exact header/cookie equality and a valid HMAC bound to issuer,
  subject, organization, the current access-token session, and its verified
  expiry. Repeated and concurrent session refreshes for the same principal and
  access token return the same token/cookie value without server-side state.
  The runtime injects the 256-bit CSRF MAC key. Missing origin or
  key configuration prevents router construction.
- Session JSON returns an opaque signed csrfToken. The matching cookie is
  __Secure-cyrene-csrf with Secure, HttpOnly, SameSite=Strict, and
  Path=/api/workspace/v1, without Domain. The access token and any
  access-token fingerprint are absent from both response and logs.
- Every JSON request and response is limited to 4 MiB. Successful Product
  responses must match closed owner schemas; undeclared fields and URL-shaped
  resource links fail closed. Declared resource references must be closed
  relative objects with an allowlisted READ key and a bounded opaque resourceId
  containing no URL or path separators. The BFF preserves a Product status and
  body only after those checks pass.
- Exchange operation 08 may accept the closed command-only `sourceEndpoint`
  selector (`product: "reactor"`, UUID `endpointId`, positive integer
  `resourceVersion`). It is not a navigable Product resource reference. The
  Exchange owner must check the scoped grant and resolve the endpoint through
  its fixed Reactor service connection; browser input never supplies a URL,
  path, or upstream operation.
- Navigator operation 13 remains unavailable to ordinary Web callers. Its
  legacy `AppendRequest.events` items are open objects, and its writer token,
  epoch fencing, and batch replay semantics require a trusted server-side
  Harness writer handoff. The current BFF has no such handoff; it retains the
  TCK row for provenance, keeps it outside the callable catalog, and returns
  BFF-owned 403 without reading or forwarding its body.
- Product `application/problem+json` responses must use the closed shared
  RFC 9457 schema. Runtime accepts only `type` and `instance` equal to
  `about:blank`, a body status matching the HTTP status, and no `resourceRef`
  or extension fields. Address-like or path-like error text fails closed with
  a BFF-owned 502; it is not relayed to the browser. Product error status and
  body are preserved only after these checks pass.
- Traceparent is validated as W3C v00, rejects zero identifiers, and is
  generated when absent. The verified value is propagated in
  WorkspaceApiRequest.traceparent; trace data never affects identity.
- BFF errors are closed RFC 9457 application/problem+json bodies and every
  route response uses Cache-Control: no-store.

## Deployment gates

The production executable binds `0.0.0.0:8080` only after startup configuration,
the PostgreSQL Workspace Directory connection, Azure AD verifier configuration,
the complete pinned Product schema bundle, and Relay mTLS/handoff configuration
have been constructed. Missing or malformed configuration, unreachable PostgreSQL,
or an incomplete/stale contract bundle aborts startup before the listener opens.
`GET /healthz` reports process liveness. `GET /readyz` deliberately stays 503:
without a real verified user token, the host cannot safely prove AAD JWKS,
Directory query permissions, or the principal-scoped Relay service trust path.
This executable is not eligible for live traffic yet. A Relay outage or a token/JWKS
failure also fails the corresponding request closed.
The three browser device-approval routes also stay unavailable (503) until a real
`DeviceEnrollmentAuthorizationPort` production composition is supplied. The optional
session-binding store below only provides durable ceremony/session binding; it is
not an approval service.

Required runtime configuration:

- `CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN`: one exact public HTTPS origin.
- `CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE`: absolute, non-symlink regular file
  containing exactly 32 nonzero key bytes; Unix group/other permissions must be off.
- `CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID` and
  `CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE`: fixed tenant UUID and API audience.
- `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`: Directory reader connection URL;
  configure its login as the restricted `cyrene_workspace_directory_reader`
  role. The adapter enforces PostgreSQL TLS `verify-full`.
- Optional `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL`: dedicated
  runtime connection URL for the durable WebAuthn HTTP session-binding store. Its
  login must use the restricted `cyrene_workspace_webauthn_http_binding_app` role;
  PostgreSQL TLS `verify-full` is enforced. If absent, approval routes remain 503.
  Schema migration uses the separate operator-only
  `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL`; the BFF
  never applies migrations at startup.
- `CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT`: read-only release bundle
  directory. Production packaging supplies the locked 36-file bundle at
  `/opt/cyrene/product-contracts`; the runtime verifies its TCK digest and all
  repository/file pins before serving.
- `CYRENE_WORKSPACE_WEB_BFF_RELAY_ENDPOINT` and
  `CYRENE_WORKSPACE_WEB_BFF_RELAY_SERVER_NAME`: one trusted HTTPS Relay origin
  and exact TLS SNI name. `CYRENE_WORKSPACE_WEB_BFF_RELAY_CA_FILE`,
  `CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_CERT_FILE`, and
  `CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_KEY_FILE` identify the Relay trust root
  and BFF workload mTLS identity. Key material is read from bounded regular
  files; the private key file must have restrictive Unix permissions.
- `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_ISSUER` and
  `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_AUDIENCE`: fixed signed handoff claims.
  `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_SIGNING_SEED_FILE` contains exactly 32
  nonzero Ed25519 seed bytes and has restrictive Unix permissions. It must match
  the verifier configured on the trusted Relay ingress.

The request path is BFF → Frontend Relay session → Workspace Connector → Product
adapter. Product endpoint manifests belong to the Connector. The BFF does not
directly resolve or connect to Product ACA endpoints. In an internal ACA
deployment, the Connector must reach each configured Product endpoint and the
BFF must reach the trusted Relay ingress; placing only the BFF in a Product's ACA
environment does not establish that path.

As of the read-only topology review on 2026-09-26, five Product ACA apps used
East Asia internal ingress, the Web app used external ingress, East Asia had no
VNet configuration or private PostgreSQL server, and no live Navigator ACA app
was found. A viable future topology must either move the Connector and required
Products into the private VNet environment, or keep the Connector near those
Products and provide a separately reviewed mTLS route to Relay hosted with the
BFF and PostgreSQL. Cross-environment reachability must be proven; it is not
implied by internal ingress or by co-locating only the BFF. No public Product
surface, public BFF ingress, placeholder secrets, or deployment is authorized by
this source change.

生产 executable 只有在启动配置、PostgreSQL Workspace Directory 连接、Azure AD verifier 配置、完整锁定的 Product schema
bundle、Relay mTLS/handoff 配置均构造成功后才会绑定 `0.0.0.0:8080`。配置缺失或格式错误、PostgreSQL 不可达、合同 bundle
不完整或过期都会在 listener 打开前终止启动。`GET /healthz` 报告进程存活。`GET /readyz` 刻意保持 503：没有真实已验证用户
token 时，host 无法安全证明 AAD JWKS、Directory 查询权限或 principal-scoped Relay 服务信任路径，因此当前 executable 不能接 live
traffic。Relay outage 或 token/JWKS failure 也会使对应请求 fail closed。
三个浏览器设备审批路由也会保持不可用（503），直到提供真实的
`DeviceEnrollmentAuthorizationPort` production composition。下面的可选 session-binding store
只负责持久化 ceremony/session 绑定，不是审批 service。

Required runtime configuration:

- `CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN`：唯一精确的 public HTTPS origin。
- `CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE`：绝对路径、非 symlink 的普通文件，恰含 32 个非零 key 字节；Unix group/other 权限必须关闭。
- `CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID` 与 `CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE`：固定 tenant UUID 与 API audience。
- `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`：Directory reader 连接 URL；运维必须配置受限的
  `cyrene_workspace_directory_reader` 登录角色，adapter 强制 PostgreSQL TLS `verify-full`。
- 可选 `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL`：WebAuthn HTTP session-binding store 的专用 runtime
  连接 URL。登录角色必须为受限的 `cyrene_workspace_webauthn_http_binding_app`；adapter 强制 PostgreSQL TLS `verify-full`。
  缺少此项时审批路由保持 503。schema migration 使用独立 operator-only
  `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_MIGRATION_DATABASE_URL`；BFF 启动时不会执行 migration。
- `CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT`：只读 release bundle 目录。生产 packaging 将锁定的 36-file bundle 放到
  `/opt/cyrene/product-contracts`；runtime 在服务前校验 TCK digest 与所有 repository/file pins。
- `CYRENE_WORKSPACE_WEB_BFF_RELAY_ENDPOINT` 与 `CYRENE_WORKSPACE_WEB_BFF_RELAY_SERVER_NAME`：可信 HTTPS Relay origin 和精确 TLS SNI。
  `CYRENE_WORKSPACE_WEB_BFF_RELAY_CA_FILE`、`CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_CERT_FILE`、
  `CYRENE_WORKSPACE_WEB_BFF_RELAY_CLIENT_KEY_FILE` 指定 Relay trust root 与 BFF workload mTLS identity。文件大小有界；private key
  文件必须使用受限 Unix 权限。
- `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_ISSUER` 与 `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_AUDIENCE`：固定签名 handoff claims。
  `CYRENE_WORKSPACE_WEB_BFF_HANDOFF_SIGNING_SEED_FILE` 恰含 32 个非零 Ed25519 seed 字节并使用受限 Unix 权限，且必须与可信 Relay
  ingress 的 verifier 相匹配。

调用链为 BFF → Frontend Relay session → Workspace Connector → Product adapter。Product endpoint manifest 属于 Connector。BFF
不会直接解析或连接 Product ACA endpoint。使用 internal ACA 时，Connector 必须能访问配置的 Product endpoint，BFF 必须能访问可信 Relay
ingress；只把 BFF 放入 Product ACA environment 并不能建立该调用路径。

截至 2026-09-26 的只读拓扑复核，五个 Product ACA app 使用 East Asia internal ingress，Web app 使用 external ingress；East Asia
无 VNet 配置或 private PostgreSQL server，也未发现 live Navigator ACA app。可行后续拓扑要么把 Connector 与所需 Product 迁入 private
VNet environment，要么让 Connector 留在 Product 附近，并为部署在 BFF/PostgreSQL 侧的 Relay 单独设计和审查 mTLS route。必须实证
跨环境可达性；internal ingress 或只共置 BFF 都不会自动提供该路径。此源码变更不授权 public Product surface、public BFF ingress、
占位 secret 或部署。
