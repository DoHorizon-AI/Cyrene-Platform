# Product adapter component boundary

Relay Host, Connector Host, and Sidecar now have separate Cargo package
versions and build targets. The shared `cy-workspace-fabric` package still owns
Platform authorization and the current Product adapter implementation. This
change does not make Product routing independently versioned.

## Current coupling

- `cy-workspace-connector-host/src/main.rs` loads
  `product-endpoints.json` with
  `load_product_endpoint_configs_for_workspace` from
  `cy-workspace-product-adapters`, then composes `ProductHttpApiAdapter` from
  `cy-workspace-fabric`. Only the private endpoint loader has moved out of Fabric.
- `src/product_adapters/{catalyst,echo,exchange,navigator,reactor,yield_api}.rs`
  map closed owner/operation enums to fixed Product paths. These modules also
  reject operation-specific body, resource-ID, kind, and idempotency-key
  mismatches; they are not just a table of strings.
- `src/product_projection.rs` owns `authorize_product_invocation`, the role
  allowlist, and Product request/response validation. `src/control_plane.rs`
  applies the Platform authorization and validation before dispatch through
  `ProductInvocationPort`.
- Navigator additionally checks that returned projection data is scoped to the
  authenticated caller's Workspace. That response check must remain a Platform
  gate if the route mapping moves.

Moving the adapter today would either duplicate/skip these checks or require a
public adapter API that accepts raw `WorkspaceCallerContext` and Product
requests. Neither is a safe component boundary. The current extraction keeps
the request path and its authorization behavior unchanged.

## Required follow-up seam

1. Add a Platform-created `AuthorizedProductInvocation` with private fields
   and a constructor available only after Platform identity, Workspace
   membership, owner/operation/kind allowlisting, body validation, and scope
   checks succeed. Narrow the adapter port so it cannot receive caller role
   authority or fabricate that authorization result.
2. Define a versioned Product operation catalog contract consumed by Connector
   host composition. Catalog entries must identify closed owner/operation and
   validated route identifiers. They must not let callers select URL, host,
   HTTP method, path segments, credentials, or authorization roles.
3. Make missing, unknown, incomplete, or unpinned catalog versions fail closed.
   Platform continues to validate requests and responses and keeps the
   private endpoint/credential manifest allowlist.
4. Migrate one simple owner mapping first. Keep Navigator's caller-scoped
   response validation in Platform; do not use browser or Product-provided
   data as identity or authorization authority.

The endpoint manifest remains a server-owned configuration of private HTTPS
endpoints and credentials. This follow-up is separate from connection process
packaging and does not enable Relay Connector authentication or change
readiness.

## Product 适配器组件边界

Relay Host、Connector Host 与 Sidecar 现已使用独立 Cargo package 版本和构建目标。共享的
`cy-workspace-fabric` 仍拥有 Platform 授权和当前 Product adapter 实现；本次变更没有让
Product 路由获得独立版本。

### 当前耦合

- `cy-workspace-connector-host/src/main.rs` 使用
  `cy-workspace-product-adapters` 的 `load_product_endpoint_configs_for_workspace` 加载
  `product-endpoints.json`，并从 `cy-workspace-fabric` 组合 `ProductHttpApiAdapter`。目前只有
  私有 endpoint loader 移出了 Fabric。
- `src/product_adapters/{catalyst,echo,exchange,navigator,reactor,yield_api}.rs` 将封闭的
  owner/operation enum 映射到固定 Product path，同时逐操作校验 body、resource ID、kind 与
  idempotency key；这些文件并非单纯的字符串表。
- `src/product_projection.rs` 拥有 `authorize_product_invocation`、role allowlist 和
  Product request/response 校验。`src/control_plane.rs` 会在经由
  `ProductInvocationPort` 分发前执行 Platform 授权与校验。
- Navigator 还会校验响应数据属于已认证 caller 的 Workspace。即使迁移路由，这项响应校验也必须保留为 Platform gate。

现在搬移 adapter 会重复或跳过这些检查，或要求公开一个能接收原始
`WorkspaceCallerContext` 和 Product request 的接口；两者都不是安全的组件边界。本次提取保持
请求路径和授权行为不变。

### 后续所需接口

1. 增加由 Platform 创建的 `AuthorizedProductInvocation`，字段私有，且只能在 Platform 身份、
   Workspace membership、owner/operation/kind allowlist、body 与 scope 检查成功后创建。收窄
   adapter port，使其不能读取 caller role authority，也不能伪造授权结果。
2. 定义由 Connector host 组合时消费的版本化 Product operation catalog 合同。条目只标识封闭
   owner/operation 与经过校验的 route ID；调用者不能选择 URL、host、HTTP method、path segment、
   credential 或授权角色。
3. 缺少、未知、不完整或 pin 不匹配的 catalog 版本必须 fail closed。Platform 继续校验请求和响应，并保留私有 endpoint/credential manifest allowlist。
4. 先迁移一个简单 owner mapping。Navigator 的 caller-scoped response validation 必须留在
   Platform；浏览器或 Product 返回数据不能成为 identity 或授权事实来源。

Endpoint manifest 仍是服务端私有 HTTPS endpoint 和 credential 配置。本后续工作独立于连接进程打包，
不启用 Relay Connector authentication，也不更改 readiness。
