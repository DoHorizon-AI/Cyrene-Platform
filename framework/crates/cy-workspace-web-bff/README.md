# Workspace Web BFF application

This crate provides the versioned Workspace Web BFF router and a production-host
composition path. The executable starts only after its required configuration,
PostgreSQL Directory, and pinned Product contract bundle load successfully. Its
readiness probe remains closed until AAD and Relay external trust paths can be
verified without a synthetic user.

本 crate 提供版本化 Workspace Web BFF router 与 production-host 装配路径。只有配置、PostgreSQL Directory 和锁定来源的
Product 合同 bundle 全部加载成功后 executable 才会启动。由于不能用合成用户验证 AAD 与 Relay 外部信任路径，就绪探针仍保持关闭。

## Composition boundary

WebBffState::new requires all providers and the source-pinned v2 catalog and policy bundle:

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
- The BFF loads the source-pinned v2 owner catalogs and OpenAPI closure from a
  release bundle. The separately pinned Platform policy is used only as a
  startup/UI preflight; Workspace control-plane authorization remains decisive.
  The owner set is taken from the immutable compatibility lock, not a compiled
  list of Products or operations. Navigator append remains in the owner catalog
  but has no default policy grant, so it fails closed.
- The browser calls the single generic v2 endpoint with `ownerId`,
  `operationId`, optional Base64 `jsonBody` bytes, optional `resourceId`, and
  optional `idempotencyKey`. The BFF resolves schemas and route metadata only
  from the pinned bundle, checks JSON scope bindings, and never accepts a URL,
  method, role, policy version, or Product credential from the browser.
- `build.rs` reads
  `tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`.
  Production Docker builds require the named BuildKit context `product-contracts`
  and compare its bundle manifest, owner catalog digests, and policy digest to
  that independent lock. Runtime then re-verifies the full manifest, owner
  source SHA map, all files, and the transitive local `$ref` closure before the
  router can start. Missing, mixed-version, stale, or incomplete artifacts fail
  closed.

WebBffState::new 必须接收全部 provider 与经过源 SHA 和 digest pin 校验的 v2 catalog/policy bundle：

- BFF 只从版本锁定 bundle 加载 Product owner catalog 与 OpenAPI 闭包。独立固定的 Platform policy 只用于启动和前置拒绝，最终授权仍由
  Workspace control plane 执行。owner 集合来自不可变 compatibility lock，不再在编译代码中维护 Product/operation 列表。Navigator append
  保留在 owner catalog，但没有默认 policy grant，因此 fail closed。
- 浏览器只调用通用 v2 endpoint，提交 `ownerId`、`operationId`、可选 Base64 `jsonBody` bytes、可选 `resourceId` 和可选
  `idempotencyKey`。BFF 只从固定 bundle 解析 schema 与路由 metadata，并验证 JSON scope binding；浏览器不能提交 URL、method、role、policy
  version 或 Product credential。
- `build.rs` 读取
  `tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`。生产 Docker build 必须提供名为
  `product-contracts` 的 BuildKit named context，并将其中 bundle manifest、owner catalog digest 与 policy digest 和独立 lock 比对。
  Runtime 再校验完整 manifest、owner source SHA map、所有文件和本地 transitive `$ref` 闭包，之后 router 才能启动。缺失、mixed-version、过期或
  不完整 artifact 均 fail closed。

WebBffState::new 必须接收全部 provider 与固定来源的 v2 catalog/policy bundle：

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
## HTTP and security behavior

- Product invocation uses only
  `POST /api/workspace/v2/workspaces/{workspaceId}/products/invocations`.
  The v1 Product route is not mounted. The existing v1 session and workspace
  discovery routes remain available to the same-origin client.
- Each request accepts exactly one server-injected
  `Authorization: Bearer <access-token>` header. Duplicate or malformed
  Authorization values and any `X-MS-TOKEN-*` header are rejected. Ingress
  must discard caller-supplied Authorization, rebuild it from the Easy Auth
  token-store value, strip Easy Auth and Cyrene identity headers, and block all
  public access that bypasses that ingress. The access token is passed only to
  the verifier and session-bound CSRF signer; it is never serialized or logged.
- The router requires one exact configured HTTPS Client origin and matching,
  valid session-bound CSRF header/cookie for every Product invocation,
  including semantic READ operations. The HMAC is bound to issuer,
  subject, organization, the current access-token session, and its verified
  expiry. Repeated and concurrent session refreshes for the same principal and
  access token return the same token/cookie value without server-side state.
  The runtime injects the 256-bit CSRF MAC key. Missing origin or
  key configuration prevents router construction.
- Session JSON returns an opaque signed csrfToken. The matching cookie is
  __Secure-cyrene-csrf with Secure, HttpOnly, SameSite=Strict, and
  Path=/api/workspace, without Domain, so the same verified session protects
  both the v1 session/discovery and v2 Product routes. The access token and any
  access-token fingerprint are absent from both response and logs.
- The HTTP envelope is limited to 6 MiB and decoded Product JSON request and
  response bodies to 4 MiB. Product responses must match the pinned owner
  schemas and JSON scope bindings; undeclared fields or values outside the
  selected organization, Workspace, or resource scope fail closed. The BFF
  preserves a Product status and body only after those checks pass.
- Navigator append remains unavailable to ordinary Web callers because the
  pinned Platform policy has no grant for it. The control plane also requires a
  trusted Harness writer handoff for append authorization; the BFF does not
  synthesize one or expose writer credentials to the browser.
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

The production executable opens its listener only after required configuration,
PostgreSQL adapters, the pinned Product bundle and policy, the device CA, and the
WebAuthn/session-binding adapters have initialized. A bare executable defaults to
`127.0.0.1:8080`; the Docker image explicitly binds `0.0.0.0:8080` for its
container ingress. Missing configuration, unreachable or unmigrated storage, or
an incomplete/stale contract bundle aborts startup before the listener opens.
`GET /healthz` reports process liveness. `GET /readyz` checks PostgreSQL schemas,
AAD discovery/JWKS, the configured Relay mTLS handshake, the signed device CA CRL,
and the loaded Product bundle without inventing a user identity. An external trust
path outage keeps readiness at 503 and requests fail closed.

The browser device-approval and WebAuthn routes are composed with durable
authorization, credential, ceremony-binding, Directory role, and restricted-CA
providers. Device start/poll/delivery-ack routes are composed from the same durable
authorization flow. The host does not run database migrations: operators must run
the storage migrator with each component's separate migration URL and provision
least-privilege application logins before starting the BFF. Real AAD user/passkey
ceremonies still require environment credentials to exercise end to end.

Required runtime configuration:

- `CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN`: one exact public HTTPS origin.
- `CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE`: absolute, non-symlink regular file
  containing exactly 32 nonzero key bytes; Unix group/other permissions must be off.
- `CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID` and
  `CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE`: fixed tenant UUID and API audience.
- `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`: Directory reader connection URL;
  configure its login as the restricted `cyrene_workspace_directory_reader`
  role. The adapter enforces PostgreSQL TLS `verify-full`.
- `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL`:
  `cyrene_workspace_device_authorization_app` runtime login for device-start,
  approval, polling, and delivery acknowledgements. This URL is also used by the
  persisted user-code attempt limiter.
- `CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL`:
  `cyrene_workspace_device_registry_app` runtime login for certificate registration
  and revocation records.
- `CYRENE_WORKSPACE_DEVICE_CA_DATABASE_URL`:
  `cyrene_workspace_device_ca_app` runtime login for signer receipts and the current
  signed CRL. Set `CYRENE_WORKSPACE_DEVICE_CA_ISSUER_ID` to its fixed issuer ID.
- `CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL`:
  `cyrene_workspace_webauthn_app` runtime login for credentials and ceremonies.
- `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL`: dedicated
  runtime connection URL for the durable WebAuthn HTTP session-binding store. Its
  login must use the restricted `cyrene_workspace_webauthn_http_binding_app` role;
  PostgreSQL TLS `verify-full` is enforced.
- `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_VERIFICATION_URI` and
  `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_KEY_VERSION`: fixed public
  verification URI and positive HMAC key version. `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_HMAC_KEY_FILE`
  points to a persistent 32-byte secret file. The Docker entrypoint stages the
  fixed `user-code-hmac-key` secret at mode 0400; it is not read from an environment
  value. Keep the key and version stable while any authorization created with it
  can still be active.
- `CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE` and
  `CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE` point to the CA key and certificate.
  The Docker entrypoint stages fixed `device-ca-signing-key.pem` and
  `device-ca-cert.pem` secret files with owner-only permissions.
- Configure each adapter's `*_MIGRATION_DATABASE_URL` only for the separate
  `cy-workspace-storage-migrator` operator action. The BFF does not apply schema
  migrations at startup and does not use an operator URL as a runtime login.
- `CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT_V2`: read-only release bundle
  directory. Production packaging supplies the bundle and separately pinned
  policy at `/opt/cyrene/product-contracts`; the runtime verifies the
  compatibility lock and every bundle file before serving.
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

生产 executable 只有在必需配置、PostgreSQL adapters、固定 Product bundle 与 policy、device CA、WebAuthn 和 session-binding
adapters 初始化成功后才会打开 listener。裸 executable 默认绑定 `127.0.0.1:8080`；Docker image 显式绑定 `0.0.0.0:8080` 以供容器 ingress
访问。缺少配置、storage 不可达或 schema 未迁移、合同 bundle 不完整或过期，都会在 listener 打开前终止启动。
`GET /healthz` 报告进程存活。`GET /readyz` 检查 PostgreSQL schemas、AAD discovery/JWKS、配置的 Relay mTLS handshake、签名 device CA CRL
和已加载 Product bundle，不伪造用户身份。外部信任路径不可用时 readiness 返回 503，业务请求 fail closed。

浏览器 device-approval 和 WebAuthn routes 已装配持久授权、credential、ceremony binding、Directory role 和受限 CA providers；device start/poll/delivery-ack
routes 也连接到同一持久授权流程。Host 不执行数据库 migrations：运维必须用各组件独立的 migration URL 运行 storage migrator，并在启动 BFF
前 provision 最小权限的 application logins。真实 AAD user/passkey ceremony 仍需要环境凭证才能端到端验证。

Required runtime configuration:

- `CYRENE_WORKSPACE_WEB_BFF_CLIENT_ORIGIN`：唯一精确的 public HTTPS origin。
- `CYRENE_WORKSPACE_WEB_BFF_CSRF_KEY_FILE`：绝对路径、非 symlink 的普通文件，恰含 32 个非零 key 字节；Unix group/other 权限必须关闭。
- `CYRENE_WORKSPACE_WEB_BFF_AAD_TENANT_ID` 与 `CYRENE_WORKSPACE_WEB_BFF_AAD_AUDIENCE`：固定 tenant UUID 与 API audience。
- `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL`：Directory reader 连接 URL；运维必须配置受限的
  `cyrene_workspace_directory_reader` 登录角色，adapter 强制 PostgreSQL TLS `verify-full`。
- `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL`：device start、approval、poll、delivery-ack 与用户码尝试限制器的
  `cyrene_workspace_device_authorization_app` runtime login。
- `CYRENE_WORKSPACE_DEVICE_REGISTRY_DATABASE_URL`：证书注册与撤销记录的 `cyrene_workspace_device_registry_app` runtime login。
- `CYRENE_WORKSPACE_DEVICE_CA_DATABASE_URL`：签发收据与当前签名 CRL 的 `cyrene_workspace_device_ca_app` runtime login；另需固定
  `CYRENE_WORKSPACE_DEVICE_CA_ISSUER_ID`。
- `CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL`：credential 与 ceremony 数据的 `cyrene_workspace_webauthn_app` runtime login。
- `CYRENE_WORKSPACE_WEBAUTHN_HTTP_SESSION_BINDING_DATABASE_URL`：WebAuthn HTTP session-binding 专用 runtime URL；角色为受限的
  `cyrene_workspace_webauthn_http_binding_app`。所有 adapters 均强制 PostgreSQL TLS `verify-full`。
- `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_VERIFICATION_URI` 与 `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_KEY_VERSION`：固定公开验证 URI
  与正整数 HMAC key version。`CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_USER_CODE_HMAC_KEY_FILE` 指向持久 32-byte secret；Docker entrypoint
  从固定的 `user-code-hmac-key` secret 挂载 stage 为 owner-only 文件，不从环境变量读取 key bytes。仍有旧授权可能有效时须保持 key/version 稳定。
- `CYRENE_WORKSPACE_DEVICE_CA_SIGNING_KEY_FILE` 与 `CYRENE_WORKSPACE_DEVICE_CA_CERTIFICATE_FILE` 指向 CA key/certificate；Docker entrypoint
  从固定 `device-ca-signing-key.pem` 和 `device-ca-cert.pem` secret 挂载 stage 为 owner-only 文件。
- 各组件 `*_MIGRATION_DATABASE_URL` 只用于单独执行 `cy-workspace-storage-migrator` operator action。BFF 不在启动时运行 schema
  migrations，也不会把 operator URL 用作 runtime login。
- `CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT_V2`：只读 release bundle 目录。生产 packaging 将 bundle 和独立 pin 的 policy 放到
  `/opt/cyrene/product-contracts`；runtime 在服务前校验 compatibility lock 与所有 bundle 文件。
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
