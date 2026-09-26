# Workspace Web BFF application

This crate provides a composable Axum router for the versioned Workspace Web
BFF contract. It deliberately has no executable or network listener. A deployable
host must wait for the trusted OIDC token-store hop, the server-side identity
Directory, the Workspace membership Directory, and an authenticated Workspace
Product API adapter to be configured together.

本 crate 提供版本化 Workspace Web BFF 合同的可装配 Axum router。当前不提供 executable 或网络 listener。只有可信 OIDC
token-store hop、服务端 identity Directory、Workspace membership Directory 与已认证 Workspace Product API adapter
共同完成配置后，才可添加部署 host。

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
- `WorkspaceApiResolver` is a required composition port. It receives only the
  selected Directory descriptor and must return a `WorkspaceApiBinding` for one
  of that descriptor's connection candidates. The binding is checked against
  the selected Workspace and candidate before dispatch. This crate does not
  provide a production endpoint-to-authority resolver or a listener; absent a
  real authenticated resolver, the application cannot be composed for service.
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
- `WorkspaceApiResolver` 是必需的 composition port。它只接收选中的 Directory descriptor，并必须返回绑定到该 descriptor
  某个 connection candidate 的 `WorkspaceApiBinding`。dispatch 前会再次校验绑定与选中 Workspace/candidate 相符。本 crate
  尚未提供生产 endpoint-to-authority resolver 或 listener；缺少真实 authenticated resolver 时，应用不能组合为可服务状态。
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

There is intentionally no default server, fake verifier, in-memory production
Directory, or 401/503 placeholder host. The current deployment still needs a
trusted same-origin ingress route, token-store access token and API scope,
production OIDC verifier configuration, an unambiguous server-side identity to
organization mapping, durable Workspace membership, owner OpenAPI validators,
and an authenticated Workspace Product gateway. The host must not bind a
public listener until it can compose these real providers and restrict network
reachability to the trusted ingress.

当前没有默认 server、fake verifier、生产 in-memory Directory 或只返回 401/503 的 placeholder host。当前部署仍需可信同源
ingress route、token-store access token 与 API scope、生产 OIDC verifier 配置、唯一的服务端 identity→organization mapping、
durable Workspace membership、owner OpenAPI validator 与 authenticated Workspace Product gateway。只有这些真实 provider
全部可装配，且网络入口限制为可信 ingress 后，host 才可监听。
