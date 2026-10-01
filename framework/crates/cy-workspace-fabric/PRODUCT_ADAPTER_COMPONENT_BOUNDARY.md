# Product adapter component boundary

Product API v2 uses owner release catalogs for route and schema selection. The
Platform control plane authorizes calls against a separately pinned policy and
passes an opaque `AuthorizedProductInvocation` to the generic HTTP adapter. The
adapter is the independent `cy-workspace-product-adapters` crate; it does not
depend on `cy-workspace-fabric` and cannot receive caller roles or choose a URL.

## Request path and ownership

- `cy-workspace-control-plane` checks the authenticated caller's organization
  and Workspace scope, membership/current authorization fence, request bounds,
  and the pinned Platform policy before it creates the opaque invocation.
- `cy-workspace-product-contracts` loads the fixed Product v2 bundle and policy
  pins, resolves each operation's route from the corresponding owner OpenAPI
  document, and validates request and response schemas and declared scope
  bindings. Unknown or unapproved operations fail closed.
- `cy-workspace-product-adapters` only sends an approved invocation over HTTPS.
  Method, route template, and path parameters come from the authorized token;
  endpoint origins and bearer credentials come from the server-owned,
  exact-owner/organization/Workspace endpoint manifest.
- Connector startup embeds the Platform release lock and loads the matching
  bundle and policy from the root-owned read-only
  `/run/cyrene-workspace-product-contracts` mount. Missing, malformed, or
  digest-mismatched inputs prevent startup.
- The Workspace API v1 Product request is explicitly unsupported. There is no
  silent v1 fallback.

## Security invariants

1. Product `operationId`, route, request/response schema, resource constraints,
   and scope bindings come from the owner release bundle pinned by the Platform
   lock. Platform policy grants remain a separately pinned Platform artifact.
2. The adapter accepts only the opaque Platform-authorized invocation and
   server-owned endpoint configuration. It never accepts raw callers, roles,
   caller-selected hosts, tokens, or catalog versions.
3. Organization, Workspace, and resource path values are injected by the
   control plane from trusted scope and catalog declarations. Redirects,
   non-HTTPS endpoints, unbounded bodies, unsupported media, and unknown route
   parameters fail closed.
4. The control plane validates Product response status, schema, response scope
   selectors, and RPC/body bounds before returning the response.

## Build and runtime inputs

The Connector image is built with the Platform repository root as Docker build
context. It compile-embeds
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`.
At runtime, the release bundle and policy are supplied at
`/mnt/cyrene-input/product-contracts` and staged read-only by the root
entrypoint. Product endpoint metadata and credential files remain a separate
private server configuration.

## Product 适配器组件边界

Product API v2 根据 owner release catalog 解析路由和 schema。Platform
control plane 按独立 pin 的 Platform policy 授权，并向通用 HTTP adapter
传递不透明的 `AuthorizedProductInvocation`。Adapter 位于独立的
`cy-workspace-product-adapters` crate，不依赖 `cy-workspace-fabric`，也无法
读取 caller roles 或选择 URL。

### 请求路径与所有权

- `cy-workspace-control-plane` 校验已认证 caller 的 organization、Workspace
  scope、membership/current authorization fence、请求大小和固定 Platform
  policy，之后才能签发不透明 invocation。
- `cy-workspace-product-contracts` 加载固定 Product v2 bundle 与 policy pins；
  按 owner OpenAPI 文档解析 operation 路由，并校验请求/响应 schema 与声明
  的 scope bindings。未知或未批准的操作 fail closed。
- `cy-workspace-product-adapters` 只通过 HTTPS 发送已授权 invocation。HTTP
  method、route template 与 path 参数来自授权 token；endpoint origin 和
  bearer credential 来自服务端拥有、精确绑定 owner/organization/Workspace
  的 endpoint manifest。
- Connector 启动时嵌入 Platform release lock，并从 root-owned、只读的
  `/run/cyrene-workspace-product-contracts` 加载匹配的 bundle 与 policy。缺失、
  格式错误或 digest 不匹配会阻止启动。
- Workspace API v1 Product request 会明确拒绝，不会静默回退到 v1。

### 安全不变量

1. Product `operationId`、route、请求/响应 schema、resource constraints 与
   scope bindings 来自 Platform lock 固定的 owner release bundle；Platform
   policy grants 是独立固定的 Platform artifact。
2. Adapter 只接受 Platform 授权的不透明 invocation 和服务端 endpoint 配置；
   不接受原始 caller、roles、caller-selected host、token 或 catalog version。
3. Organization、Workspace、resource path 值由 control plane 根据可信 scope
   和 catalog 声明注入。Redirect、非 HTTPS endpoint、超限 body、不支持的媒体
   类型及未知路由参数均 fail closed。
4. Control plane 在返回响应前校验 Product status、schema、response scope
   selectors 以及 RPC/body bounds。

### 构建与运行输入

Connector 镜像使用 Platform 仓库根目录作为 Docker build context，并编译嵌入
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`。
运行时 release bundle 与 policy 从 `/mnt/cyrene-input/product-contracts` 提供，
由 root entrypoint 暂存为只读文件。Product endpoint 元数据和 credential 文件仍
是独立的服务端私有配置。
