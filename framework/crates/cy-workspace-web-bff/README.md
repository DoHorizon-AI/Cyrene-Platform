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
- WorkspaceProductGateway receives a verified principal plus typed
  WorkspaceApiRequest. Its runtime adapter must call the authenticated
  Workspace API path and establish Workspace caller context after membership
  verification. The legacy unauthenticated WorkspaceApi::handle is not an
  acceptable adapter.
- ProductOperationCatalog must contain exactly one matching owner OpenAPI
  operation for each canonical projection row. Its operation key, owner,
  Product operationId, and semantic kind must match the TCK. The BFF does not
  maintain a second owner or permission table.

build.rs reads
contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv.
The generated rows are checked against the compiled
WorkspaceProductApiOperation enum at router composition and in crate tests.
The runtime composition root must compile request, path, and response schema
validators from the Product OpenAPI documents and fail closed if a required
operation contract is absent or disagrees with its TCK row.

WebBffState::new 必须接收全部 provider 与 canonical operation catalog：

- WebPrincipalVerifier 使用固定 issuer、audience、JWKS key、token lifetime 与 scope rule 校验 access token；
  router 只消费其不可变 VerifiedWebPrincipal getter，绝不从 X-MS-CLIENT-PRINCIPAL 或其他未签名身份 header
  构造 principal。
- WorkspaceDirectory 提供当前 member-scoped Workspace descriptor；router 只投影
  workspaceId、organizationId 和 displayName。
- WorkspaceProductGateway 接收 verified principal 与 typed WorkspaceApiRequest。其 runtime adapter 必须调用
  authenticated Workspace API path，并在 membership 检查后建立 Workspace caller context。legacy 未认证的
  WorkspaceApi::handle 不能作为 adapter。
- ProductOperationCatalog 对 canonical projection row 的每个 operation 必须恰有一个匹配的 owner OpenAPI operation。
  operation key、owner、Product operationId 和语义类型必须与 TCK 一致。BFF 不维护第二份 owner 或 permission table。

build.rs 从 contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv 读取 projection rows，并在 router
composition 与 crate tests 中和生成的 WorkspaceProductApiOperation enum 对齐。Runtime composition root 必须从 Product
OpenAPI 文档编译 request、path、response schema validator；缺少 operation 合同或与 TCK 不符时 fail closed。

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
- Every JSON request and response is limited to 4 MiB. Owner request and
  response schemas validate without rewriting Product JSON bytes. Declared
  resource references must be closed relative objects with an allowlisted READ
  key and one opaque resourceId; missing response schema or unsafe reference
  returns 502 with no Product body.
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
