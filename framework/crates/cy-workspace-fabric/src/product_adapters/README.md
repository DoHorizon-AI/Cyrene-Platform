# Product HTTP adapters / Product HTTP 适配器

This directory maps the Workspace's closed Product operation keys to fixed
Product-owned HTTP routes. The shared client requires private HTTPS endpoint
and bearer credential configuration for each owner, organization, and
Workspace. It resolves endpoints from the authenticated caller context and
fails closed on an organization or Workspace mismatch. It rejects redirects,
applies timeouts, and caps JSON request and response bodies at 4 MiB. It does
not persist Product resources or allow callers to select an endpoint, path, or
HTTP method.

Startup can load these entries with `load_product_endpoint_configs` from a
server-owned JSON manifest and a private secret directory. The manifest names
only credential basenames; bearer values stay in separate files and are never
included in diagnostics. The manifest must be canonical and regular, and the
secret directory and files must be owned by the same service identity with
private permissions (directory `0700`, credential files `0600`). Each bearer
must be 32–4096 printable ASCII bytes, unique across configured scopes, and
must not include a newline. ACA Key Vault volume mounts may use symlinks or
broader file modes, so copy the needed values into a service-owned private
directory before starting this loader.

```json
{
  "version": 1,
  "endpoints": [
    {
      "owner": "CATALYST",
      "organizationId": "org-1",
      "workspaceId": "workspace-1",
      "baseUrl": "https://catalyst.internal/",
      "credentialFile": "catalyst-workspace-1"
    }
  ]
}
```

The dispatcher enables Catalyst, Yield, Echo, Exchange, Reactor, and Navigator
through fixed owner routes and caller-scoped endpoint credentials. Catalyst,
Yield, Echo, and Reactor use `/internal/workspace/v1/...`; Exchange uses
protected `/api/v1/workspace/...` aliases. Reactor permits only GET and POST
`/internal/workspace/v1/model-imports`; its owner checks the configured
workspace-to-serving-binding grant for POST and the adapter preserves an owner
403 response. Legacy Reactor routes are rejected.

Navigator operation 11 sends the closed read request to
`POST /api/v1/workspace-snapshots`; its nested Product reads are limited to the
candidate contract's fixed internal routes and Exchange workspace alias. A
successful response must match the closed `WorkspaceSnapshot` shape, including
its source-operation labels and non-navigable digest summaries. Operation 12
uses `GET
/internal/workspace/v1/workspaces/{workspace_id}/sessions/{session_id}` and
accepts only the closed `WorkspaceSessionSummary` and `ProductMetadata` fields;
the returned Workspace and session identifiers must match the authenticated
caller and requested resource. Both operations are READ-only. The private
manifest supplies one Navigator bearer for an exact organization and
Workspace; Navigator maps the bearer labels for both endpoints to the same
server-owned principal. The browser bearer is never forwarded or accepted by
this adapter, and the scoped bearer cannot be reused for another configured
scope. Operation 13 always returns permission denied, even when an endpoint is
configured, until a trusted Harness writer handoff exists.

The Workspace adapter does not invent or forward a user as a Product actor,
and the role policy grants only the exact configured user commands. Production
composition and trusted caller audit provenance are still incomplete, so this
aggregate is not yet a production-ready path.

本目录将 Workspace 封闭的 Product 操作键映射到固定的 Product HTTP 路由。公共 client
要求服务端按 owner、organization 与 Workspace 私有配置 HTTPS 端点及 bearer credential。
resolver 根据可信调用者上下文精确匹配范围，organization 或 Workspace 不匹配时 fail closed。
它禁用重定向、设置超时，并将 JSON 请求与响应限制为 4 MiB；不会持久化 Product 资源，
也不允许调用方选择端点、路径或 HTTP method。

启动时可通过 `load_product_endpoint_configs` 从服务端 JSON manifest 与私有 secret 目录加载配置。
manifest 只包含 credential 文件名，bearer 值保存在单独文件中，不会进入诊断输出。manifest 必须是
规范路径上的普通文件；secret 目录与文件必须由同一服务身份拥有且权限私有（目录 `0700`、文件 `0600`）。
每个 bearer 长度为 32–4096 个可打印 ASCII 字节，在不同 scope 间唯一，且不得带换行。ACA Key Vault
volume mount 可能使用符号链接或更宽权限；启动 loader 前须将所需值复制到服务拥有的私有目录。

| File | Responsibility |
| --- | --- |
| `http.rs` | Private endpoint resolution, injected transport, bounded HTTP and JSON handling. |
| `endpoint_manifest.rs` | Strict private-file loader for scoped endpoint and bearer configuration. |
| `catalyst.rs` | Fixed Catalyst dataset route mappings. |
| `yield_api.rs` | Fixed Yield draft and run route mappings. |
| `reactor.rs` | Fixed Reactor model import route mappings. |
| `exchange.rs` | Fixed Exchange gateway route mappings. |
| `echo.rs` | Fixed Echo evaluation suite route mappings. |
| `navigator.rs` | Fixed Navigator read routes; event append remains denied. |
| `mod.rs` | Six-owner `ProductInvocationPort` router and module composition. |

Suggested reading order: `mod.rs`, `http.rs`, then the owner adapter matching
the operation under review.

建议阅读顺序：先读 `mod.rs`，再读 `http.rs`，最后按待审查操作阅读对应 owner 适配器。

当前 dispatcher 通过固定 owner 路由与调用者范围 endpoint credential 启用 Catalyst、Yield、Echo、Exchange、Reactor
和 Navigator。Catalyst、Yield、Echo 与 Reactor 使用 `/internal/workspace/v1/...`；Exchange 使用受保护的
`/api/v1/workspace/...` 别名。Reactor 仅允许 GET 与 POST
`/internal/workspace/v1/model-imports`；owner 会检查 POST 对应的 workspace-to-serving-binding grant，适配器原样
保留 owner 的 403 响应，并拒绝旧 Reactor 路由。

Navigator 操作 11 将封闭的只读请求发送到 `POST /api/v1/workspace-snapshots`；内部 Product reads 仅允许候选合同
中的固定路由和 Exchange workspace 别名。成功响应必须符合封闭的 `WorkspaceSnapshot` 结构，包括固定来源 operation
标签和不可导航的摘要 digest。操作 12 使用
`GET /internal/workspace/v1/workspaces/{workspace_id}/sessions/{session_id}`，只接受封闭的
`WorkspaceSessionSummary` 与 `ProductMetadata` 字段；返回的 Workspace 和 session ID 必须匹配已认证调用者与请求资源。
两个操作都只读。私有 manifest 为精确的 organization 与 Workspace 配置一个 Navigator bearer；Navigator 服务将两个 endpoint
的 bearer scheme label 映射到同一个服务端 principal。此适配器不会转发或接受浏览器 bearer，scope credential 也不能用于其他
配置 scope。即使配置 endpoint，操作 13 仍始终返回 permission denied，直到接入可信 Harness writer handoff。

Workspace adapter 不会伪造或转发用户作为 Product actor，角色策略只授予精确配置的用户命令。生产组合与可信调用者审计来源仍未完成，
因此此 aggregate 尚不是可用于生产的路径。
